# Per-CPU campaign, the percpu half against the baseline half (lane D, 2026-09-20)

Plan Phase 2.5 task 8's acceptance rows, `bench/percpu-campaign.sh`
(`make percpu-campaign MODE=percpu LANE=lane-d DATE=2026-09-20`), run today
on the lane's two VMs from `main` 0c60b1a (library code unchanged since
856f103: task 8 at 14c7fad..94fa9aa plus lane A's fast-path commits, the
PAC gate b43532f, lane B's floor fixes, lane C's key baa8bc2). The baseline
half ran 2026-09-19 at 675f357 (before task 8; the key accepted and
ignored): `2026-09-19-{aarch64,x86_64}-lane-d-percpu-baseline.md`. Lane D is
measurement only; nothing under `src/` changed. The mode is the human's
decision (PROGRESS.md "Notes for the human", the 2026-09-20 task 8
bullets); this file is the rows.

Hosts: `challoc-lane-d-arm64` (c4a-standard-16, Neoverse V2, 7.0.0-1011-gcp,
feature line `... rseq=yes rseq_size=33 mm_cid=yes ... pac=yes ...`) and
`challoc-lane-d-amd64` (c4-standard-16, Xeon 8581C, the same kernel,
`rseq=yes rseq_size=33 mm_cid=yes`), created, provisioned, synced, run
04:47..07:05 UTC (aarch64 7258 s, x86_64 7959 s) and deleted. Files:

- `2026-09-20-<arch>-lane-d-percpu-percpu.md`: the campaign summary, one
  section per step with the lines that carry the verdict;
- `...-percpu-percpu-ab.md` (the same-run triple `default=tree`,
  `percpu=tree:percpu_arena:percpu`, `jemalloc`, five rounds),
  `...-ruby.md`, `...-ruby-soak.md` (+ CSVs, mstats), `...-ruby-idle.md`
  (+ CSVs, mstats), `...-percpu-percpu-soak/2026-09-20-<arch>-valkey-soak.md`
  (+ CSVs);
- `...-percpu-percpu-test.log`: the supplement to the `test` step (section
  1), `make test` under the key with one environment test skipped, and the
  isolation of the one real failure.

In every step but `ab`, challoc runs under `CHALLOC_CONF=percpu_arena:percpu`
(forwarded by the Makefile into every container; jemalloc does not read
it); in `ab` the variant's own `MALLOC_CONF` carries the key, so `default`
and `percpu` are the same run. "Baseline" below is the 2026-09-19 default
at 675f357; "default" is today's same-run default at 0c60b1a.

## 0. Headline rows

| row (positive = challoc worse) | criterion (plan) | aarch64 (C4A) | x86_64 (C4) | verdict |
|---|---|---|---|---|
| mstress peak RSS, default vs jemalloc | Phase 0: within 10% (the accepted Phase 1.5 row) | 320.5 vs 291.7 MiB, **+9.9%** (baseline +9.4%) | 337.6 vs 284.1 MiB, **+18.8%** (baseline +16.7%) | default unchanged |
| mstress peak RSS, `percpu` vs jemalloc | task 8: "the x86_64 RSS row re-measured under the mode" | 319.9 MiB, **+9.7%** | 316.8 MiB, **+11.5%** (20.8 MiB, 6.2%, below the default) | the mode takes 7 points off the x86_64 row; still outside 10% |
| mstress time, `percpu` vs default | cost recorded | +1.6% (0.379 vs 0.373 s) | +0.3% (0.640 vs 0.638 s) | within the row's spread |
| larson64 (64 threads on 16 CPUs) throughput / RSS, `percpu` vs default | cost recorded | **+20.8%** slower / RSS **-43.5%** (317.0 vs 561.0 MiB) | **+29.6%** / **-43.1%** (269.7 vs 474.3 MiB) | the mode's shape: RSS follows CPUs, throughput pays |
| larson64, `percpu` vs jemalloc | Phase 0 set is not this row | +26.4% slower / RSS -39.8% | +42.5% / RSS -35.0% | |
| valkey1 / valkey1-free, `percpu` vs default | cost recorded | +5.4% / +9.4% | **+34.1% / +24.1%** | lane A's attribution stands (`2026-09-19-x86_64-task8-percpu-fastpath.md`: +37.8% on the C4) |
| calloc-zero campaign, `check_zero:true,percpu_arena:percpu` | 100/100 clean | 100 ok / 0 failed, 10/10 standalone | 100 ok / 0 failed, 10/10 standalone | **met** (the api:: load's own failures are section 1's, not calloc's) |
| container tests under the key (churn 512m/256m, lazy, oscillation 300 s) | Phase 2: pass | every `OUTCOME ... verdict=ok`; oscillation 98 cycles, peak 501 MiB | every ok; 96 cycles, 502 MiB | **met** |
| `make test` under the key | task 8: suites green | 399 + 115/2 + 23: `c_percpu_both_ways` (the environment, section 1.1) and `c_tls_threads_with_pac_pointers` (**SIGILL, real**, section 1.2) | 399 + 116/1 + 23: `c_percpu_both_ways` only | **not green on aarch64**: `pac_pointers:true` + `percpu_arena:percpu` faults on FEAT_FPAC |
| Valkey suite under the key | Phase 1: green | `unit/memefficiency` "All tests passed without errors" (438 s) | memefficiency (457 s) and the full suite (967 s): "All tests passed without errors" | **met** |
| Valkey soak under the key (20 min per allocator, same run) | Phase 1: throughput and p99 at least equal, peak RSS within 5%, settle within 10% | write vb -1.3% / memtier +3.1% req/s, p99 -0.7% / **+14.1%**; delete vb +4.6% / memtier **+11.1%**, p99 +4.6% / **+30.1%**; peak RSS +0.2%; settle +1.5% | write -4.0% / +0.9%, p99 -3.7% / 0.0%; delete +3.1% / +2.1%, p99 +3.4% / +1.0%; peak +0.3%; settle -3.8% | RSS met on both; throughput behind in the delete phase on both (the default's residue was +1.4..1.5% on aarch64, ahead on x86_64) |
| Ruby rows under the key (medians vs jemalloc) | Phase 1: at or ahead, or attributed | startup +8.1%, gem_list +5.6%, hash_aref_str +0.9%, erb_render -7.9%, the rest within 0.3%; RSS below jemalloc on every row but gc_churn (+4.6%) | startup +2.4%, gem_list +1.7%, hash_aref_str +1.3%, so_nbody +0.8%, erb_render -12.1%, gc_churn -2.6%; RSS below on every row | as the default (startup and gem_list are the M6 init rows) |
| Ruby `test-all` under the key | Phase 1: passes or unrelated failures | 32858 tests, 7 failures + 2 errors: the nine environmental tests | 32845 tests, the same nine | **met** |
| Ruby soak, 16 workers, 600 s, RSS at t=600 vs jemalloc | task 8's target shape | **186.8 vs 169.0 MiB (+10.5%)**, peak 187.8 vs 199.0 (-5.6%); `Arenas: 16` (baseline 397.0 vs 160.6, +147%, `Arenas: 17`) | **207.7 vs 140.2 MiB (+48.1%)**, peak 209.9 vs 179.1 (+17.2%); `Arenas: 16` (baseline 413.0 vs 167.0, +147%) | the mode halves the resident set; x86_64 still +48% at the end |
| Ruby idle soak, 16 workers + 48 idle threads, 300 s, peak RSS vs jemalloc | "RSS bounded by cores, not threads" | **112.0 vs 191.2 MiB (-41.4%)**; `Arenas: 16 (64 automatic)` (baseline 415.6 vs 191.5, +117%, `Arenas: 64`) | **158.5 vs 165.7 MiB (-4.3%)** (baseline 462.8 vs 115.3, +301%) | **met**: 16 arenas on 16 CPUs whatever the thread count |

## 1. `test`: the suite under the key

The campaign's `test` step is `make test` with `CHALLOC_CONF=percpu_arena:percpu`
in the container's environment. It stopped after the unit tests, 399/1:
`config::linux_tests::load_with_a_clean_environment_gives_the_defaults`
asserts `CHALLOC_CONF` is unset ("the test assumes a clean environment"),
so the api:: tests and the C tests of the target never ran (both VMs,
`rc=2 in 26..28 s`). The supplement (`...-percpu-percpu-test.log`, part 2)
ran the target's steps with that one test skipped:

| host | unit tests | api:: tests | cgroup limit tests | churn / lazy / pagemap steps | failures |
|---|---|---|---|---|---|
| aarch64 | 399/0 | **115/2** | 23/0 | all rc=0 | `api::ctests::c_percpu_both_ways`, `api::ctests::c_tls_threads_with_pac_pointers` |
| x86_64 | 399/0 | **116/1** | 23/0 | all rc=0 | `api::ctests::c_percpu_both_ways` |

The calloc-zero campaign's api:: load (five `cargo test api::` runs per
host, under the same environment) saw the same two / one failures.

### 1.1 `c_percpu_both_ways`: the environment, not the mode

The test runs `tests/c/percpu` twice with `MALLOC_CONF=abort_conf:true,percpu_arena:{percpu,off}`;
`CHALLOC_CONF` is read after `MALLOC_CONF` and wins, so the `off` direction
ran on: `FAIL /src/tests/c/percpu.c:113: opt.percpu_arena is percpu,
expected off` / `percpu_arena:off but the mode is on` (the `percpu`
direction passed: `mode percpu enabled=1 ids=16 headers=193 cached=46211
aborts=1 after 64 threads x 20000 ops`). Same on x86_64. A suite run under
`CHALLOC_CONF` cannot test the `off` way; either the test clears the
variable for its child (`Command::env_remove`) or the campaign's `test`
step runs the suite without the key and the key's own both-ways test
covers the mode. Owner: lane A (`src/api/ctests.rs`), or lane D for the
campaign script's side.

### 1.2 `c_tls_threads_with_pac_pointers`: `pac_pointers:true` with `percpu_arena:percpu` faults on FEAT_FPAC

`tests/c/tls_threads` (64 threads, distinct heaps) under
`MALLOC_CONF=pac_pointers:true,abort_conf:true` plus the environment's
`percpu_arena:percpu` died with status 132 (`SIGILL`) on the C4A, empty
stdout and stderr. Isolated on the idle VM after the campaign (part 1 of
the supplement, `MALLOC_CONF` alone, no `CHALLOC_CONF`), three runs per
combination:

| `MALLOC_CONF=abort_conf:true,...` | aarch64 (C4A, FEAT_FPAC) | x86_64 (C4, PAC compiled out) |
|---|---|---|
| `pac_pointers:true,percpu_arena:percpu` | **rc=132, 132, 0** (`Illegal instruction (core dumped)`) | 0, 0, 0 |
| `percpu_arena:percpu,pac_pointers:true` | **0, 132, 0** | 0, 0, 0 |
| `percpu_arena:percpu` | 0, 0, 0 | 0, 0, 0 |
| `pac_pointers:true` | 0, 0, 0 | 0, 0, 0 |
| `percpu_arena:off,pac_pointers:true` | 0, 0, 0 | 0, 0, 0 |
| `tests/c/percpu percpu` with `pac_pointers:true` | **132, 132** | 0, 0 (`ok percpu`) |
| `tests/c/percpu percpu` with `pac_pointers:false` | 0, 0 (`aborts=4`, `aborts=1`) | 0, 0 |

Either key alone is clean; together they fault on the host with FEAT_FPAC
(where a failed `autda` faults at the instruction), intermittently under
tls_threads and every time under the percpu test's 64 x 20000 operations.
The reading: a cache entry crosses between the signed thread-cache path
(signed with the cache header's address as the modifier) and the per-CPU
path (a different header, or unsigned), so an `autda` runs on a pointer
signed under another modifier. Both keys are opt-in (`pac_pointers`
default off since 2026-09-19, `percpu_arena:auto` keeps thread caches), so
no default is affected, but the combination is a shipped-library fault,
not a test bug. Owner: lane A (`src/heap.rs`, the per-CPU pop/push and
flush paths). Until it is fixed, task 8's "suites green" under the key
holds only with `pac_pointers` off.

## 2. `ab`: the Phase 0 set, larson64 and the Valkey model rows

`2026-09-20-<arch>-lane-d-percpu-percpu-ab.md`, medians of five rounds,
one run with three variants. The `percpu vs default` column is the mode's
cost against the same-run default; `default vs jemalloc` is the Phase 0
criterion at 0c60b1a (positive = challoc worse):

| row | aarch64 percpu vs default | aarch64 default vs jemalloc | aarch64 percpu vs jemalloc | x86_64 percpu vs default | x86_64 default vs jemalloc | x86_64 percpu vs jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| larson1 | +7.2% | +7.3% | +15.6% | +4.6% | -0.4% | +4.2% |
| larson10 | +11.7% | +6.0% | +18.8% | +20.5% | +3.0% | +24.3% |
| larson16 | +12.9% | +4.0% | +17.5% | +21.8% | +8.3% | +32.8% |
| larson64 | +20.8% | +4.4% | +26.4% | +29.6% | +9.0% | +42.5% |
| xmalloc | +18.2% | -20.7% | -2.1% | +11.7% | -0.8% | +10.8% |
| alloc-test1 | +5.7% | +6.0% | +12.3% | +0.1% | +8.9% | +10.0% |
| alloc-testN | +5.4% | +3.7% | +9.4% | +4.3% | +9.4% | +15.2% |
| cfrac | +7.4% | +0.9% | +8.3% | +4.3% | +4.9% | +9.6% |
| espresso | +3.3% | -2.4% | +0.9% | -0.0% | +1.7% | +1.7% |
| mstress (time) | +1.6% | -72.1% | -41.0% | +0.3% | -69.0% | -40.6% |
| glibc-simple | +16.8% | -15.9% | +0.7% | +5.0% | +4.1% | +9.6% |
| redis | +1.7% | -21.6% | -16.4% | +2.2% | -25.5% | -18.5% |
| valkey1 | +5.4% | +1.2% | +6.7% | +34.1% | +1.2% | +35.8% |
| valkey1-free | +9.4% | -0.6% | +8.7% | +24.1% | -10.1% | +12.7% |

RSS (MiB, percpu / default / jemalloc): aarch64 mstress 319.9 / 320.5 /
291.7, larson64 317.0 / 561.0 / 527.0, larson16 167.8 / 166.4 / 206.4,
xmalloc 78.5 / 53.5 / 78.0; x86_64 mstress 316.8 / 337.6 / 284.1, larson64
269.7 / 474.3 / 414.9, larson16 108.3 / 119.6 / 127.8, xmalloc 57.7 / 52.9
/ 91.8. Under the mode xmalloc's retained memory grows to jemalloc's on
the C4A (53.5 -> 78.5 MiB; lane A's note on the per-CPU flush's copies),
and larson's falls by 40% at 64 threads.

Two readings on the default column at 0c60b1a against the baseline at
675f357: (1) aarch64 larson1 42.4 -> 41.8 M ops/s against a jemalloc of
45.0 -> 45.1 (the row moved from -5.8% to +7.3% in the ab's sign
convention, i.e. from 5.8% to 7.3% behind), the drift the coordinator
attributed to 4864d4b's dispatch (`2026-09-20-aarch64-larson1-drift.md`,
+9.1 instructions per operation); (2) every x86_64 timed row is slower
today for both allocators (alloc-test1 2.851 -> 3.401 s and jemalloc 2.703
-> 3.097; cfrac 3.650 -> 4.180 and 3.461 -> 3.977; the Ruby rows'
sorted lists are bimodal for both allocators, e.g. so_binary_trees 3.98 /
3.99 / 4.30 / 4.31 / 4.39), so today's C4 host ran slower than yesterday's
and only the same-run ratios compare. Phase 0 time criterion at 0c60b1a
(default within 10% of jemalloc): aarch64 worst larson1 +7.3%,
alloc-test1 +6.0%, larson10 +6.0%; x86_64 worst alloc-testN +9.4%,
larson64 +9.0% (not a criterion row), alloc-test1 +8.9%, larson16 +8.3%;
every other row ahead or within 5%: **PASS** on both, x86_64 with less
margin than yesterday (larson16 +8.9%, alloc-testN +7.5%).

## 3. `calloc`, `cgroup`, `valkey-suite`

`calloc`: `bench/probes/calloc-zero-campaign.sh` with `CONFIGS="percpu:check_zero:true,percpu_arena:percpu default:check_zero:true"`,
100 x `stress 8 50 10` per configuration under the api:: load, then 10 x
`stress 16 100 20` standalone under `CHALLOC_CONF=percpu_arena:percpu`:

```
aarch64: RESULT percpu: 100 ok, 0 failed, 100 runs of stress 8 50 10, 27 s, conf=check_zero:true,percpu_arena:percpu
         RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 27 s, conf=check_zero:true
         RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 5 s
x86_64:  RESULT percpu: 100 ok, 0 failed, 100 runs of stress 8 50 10, 41 s, conf=check_zero:true,percpu_arena:percpu
         RESULT default: 100 ok, 0 failed, 100 runs of stress 8 50 10, 43 s, conf=check_zero:true
         RESULT standalone: 10 ok, 0 failed, 10 runs of stress 16 100 20, 8 s
```

(`RESULT load: 5 runs of cargo test api::, 5 failed` on both: section 1's
two / one tests, under the campaign's environment.) The zero-prefix
protocol holds under the mode; the baseline half read the same 100/100 and
10/10.

`cgroup` under the key, both hosts: churn `purge 400 100` 403 -> 16 MiB in
55 / 46 ms (`keep` control 317 / 314 MiB), `purge 200 50` 202 -> 9 / 10 MiB
in 81 ms (`keep` 161), lazy 202 -> 202 (35 lazy purges, `pglazyfree_pages`
48571) and eager -> 12 / 13, oscillation 98 / 96 cycles in 300 s, peak
501 / 502 MiB, `memory.events` all zero; every `OUTCOME ... verdict=ok
oom_kill=0`. The same numbers as the baseline half.

`valkey-suite` under the key: `make valkey STAGE=memefficiency` "All tests
passed without errors" on both (438 s / 457 s), `STAGE=full` on x86_64
"All tests passed without errors" (967 s), `malloc=jemalloc-5.3.0-challoc`.

## 4. The Valkey soak under the key

`2026-09-20-<arch>-lane-d-percpu-percpu-soak/2026-09-20-<arch>-valkey-soak.md`:
challoc under `CHALLOC_CONF=percpu_arena:percpu` and the bundled jemalloc,
1200 s of load each, the Phase 1 protocol, the same run (the baseline
half's cached jemalloc soak did not survive yesterday's VM deletion, so
today's jemalloc is fresh, and the table also carries the per-phase
cgroup `memory.peak` of task 17). Baseline = yesterday's default at
675f357, from its own run:

| measure (challoc vs jemalloc, positive = worse) | aarch64 percpu | aarch64 baseline | x86_64 percpu | x86_64 baseline |
|---|---:|---:|---:|---:|
| write: valkey-benchmark req/s | -1.3% | -4.8% | -4.0% | +1.5% |
| write: valkey-benchmark p99 | -0.7% | -5.1% | -3.7% | -1.3% |
| write: memtier req/s | +3.1% | -2.3% | +0.9% | +4.9% |
| write: memtier p99 | **+14.1%** | -4.0% | +0.0% | +1.7% |
| delete: valkey-benchmark req/s | **+4.6%** | +1.4% | **+3.1%** | +0.1% |
| delete: valkey-benchmark p99 | +4.6% | +2.3% | +3.4% | -3.5% |
| delete: memtier req/s | **+11.1%** | +1.5% | +2.1% | -2.1% |
| delete: memtier p99 | **+30.1%** | +1.0% | +1.0% | -4.2% |
| peak RSS (VmHWM) | +0.2% | +1.1% | +0.3% | +0.4% |
| RSS after settle | +1.5% | +0.8% | -3.8% | -3.6% |
| allocator_frag_ratio after settle | 1.78 vs 1.81 | 1.81 vs 1.80 | 1.75 vs 1.78 | 1.80 vs 1.82 |

Absolute (aarch64, percpu / jemalloc): write 920471 / 908954 req/s
valkey-benchmark, 924892 / 953954 memtier; delete 1389863 / 1453507 and
1153875 / 1281876; peak 575.2 / 574.1 MiB; settle 131.8 / 129.9 MiB.
x86_64: write 753753 / 723408 and 921696 / 929724; delete 1335966 / 1377653
and 1216804 / 1242349; peak 575.8 / 573.9; settle 126.2 / 131.2.

Criteria: peak RSS within 5% and settle within 10% met on both;
"throughput and p99 at least equal" not met in the delete phase on either
host (the aarch64 memtier delete p99 +30.1% is the largest miss any soak
has recorded), and not in the write phase's memtier p99 on aarch64. The
default's aarch64 delete residue (+1.4..1.5%) triples under the mode; the
x86_64 write phase, behind under the default yesterday (+1.5 / +4.9%), is
ahead under the mode today (-4.0 / +0.9%), a reading the two hosts' day to
day spread covers (section 2's note on the C4). One round each; the
delete-phase gap fits lane A's attribution (the scheduled pop's cold path
and the per-CPU flush's copies on a free-heavy phase).

## 5. The Ruby rows, `test-all`, the soaks

Rows (medians of five, challoc under the key vs jemalloc; RSS = maxrss):

| row | aarch64 percpu | aarch64 baseline | x86_64 percpu | x86_64 baseline |
|---|---:|---:|---:|---:|
| startup | +8.1% (0.040 vs 0.037 s) | +5.4% | +2.4% | +2.5% |
| so_binary_trees | +0.1% | within 1% | +0.3% | within 2% |
| hash_aref_str | +0.9% | within 1% | +1.3% | within 2% |
| array_sample_100k__1k | -0.2% | | -0.2% | |
| array_sort_block | 0.0% | | +0.3% | |
| so_nbody | +0.2% | | +0.8% | |
| app_fib | +0.3% | | -0.4% | |
| erb_render | -7.9% (0.889 vs 0.965) | -13.9% (0.833) | -12.1% (0.761 vs 0.866) | -19.5% |
| gem_list | +5.6% | +3.7% | +1.7% | within 2% |
| gc_churn | -0.4% | -2.7% | -2.6% | -1.1% |
| RSS | below jemalloc on every row but gc_churn (+4.6%, 382 vs 366 MiB) | the same (+6.1%) | below on every row (gc_churn -0.3%) | below but gc_churn (+4.9%) |

erb_render's lead shrinks under the mode (aarch64 0.833 -> 0.889 s against
an unchanged jemalloc), the one Ruby time row the mode visibly costs; the
rest is inside the rows' spread.

`test-all` under the key: aarch64 `32858 tests, 6587762 assertions, 7
failures, 2 errors, 409 skips`; x86_64 `32845 tests, 6587987 assertions, 7
failures, 2 errors, 407 skips`. The nine names are the Phase 1 / Phase 2
set on both: `OpenSSL::TestSSL#test_connect_works_when_setting_dh_callback_to_nil`,
`#test_tlsext_hostname`, `#test_tmp_dh`, `#test_tmp_dh_callback`,
`OpenSSL::TestProvider#test_openssl_legacy_provider`,
`TestFileUtils#test_rm_r_no_permissions`, `TestTmpdir#test_world_writable`,
`RDocParserTest#test_class_for_forbidden`,
`TestFiddle#test_dlopen_linker_script_group_linux`. No abort, no
`challoc:` line.

Ruby soak (16 workers, 600 s, full GC every 30 s, `GC.compact` at 300 s;
`...-ruby-soak.md`):

| measure (MiB) | aarch64 percpu / jemalloc | aarch64 baseline / jemalloc | x86_64 percpu / jemalloc | x86_64 baseline / jemalloc |
|---|---|---|---|---|
| peak sampled VmRSS | 187.8 / 199.0 (-5.6%) | 403.6 / 198.4 (+103%) | 209.9 / 179.1 (+17.2%) | 419.1 / 184.1 (+128%) |
| at minute 1 | 70.7 / 98.6 (-28%) | 285.2 / 96.7 (+195%) | 93.1 / 73.2 (+27%) | 308.9 / 72.5 (+326%) |
| after GC.compact (t=300) | 110.2 / 130.6 (-16%) | 323.4 / 130.3 (+148%) | 146.9 / 116.5 (+26%) | 362.8 / 116.2 (+212%) |
| at minute 10 (t=600) | **186.8 / 169.0 (+10.5%)** | 397.0 / 160.6 (+147%) | **207.7 / 140.2 (+48.1%)** | 413.0 / 167.0 (+147%) |
| lowest after a full GC | 66.6 / 91.8 | 279.3 / 93.5 | 91.8 / 71.2 | 302.0 / 72.5 |
| `Arenas:` in the report | 16 (64 automatic) | 17 (64 automatic) | 16 | 17 |

Ruby idle soak (16 workers + 48 threads that allocate once and wake every
5..10 s, 300 s, `GC.compact` at 150 s; `...-ruby-idle.md`):

| measure (MiB) | aarch64 percpu / jemalloc | aarch64 baseline / jemalloc | x86_64 percpu / jemalloc | x86_64 baseline / jemalloc |
|---|---|---|---|---|
| peak sampled VmRSS | **112.0 / 191.2 (-41.4%)** | 415.6 / 191.5 (+117%) | **158.5 / 165.7 (-4.3%)** | 462.8 / 156.0 (+197%) |
| at minute 1 | 72.7 / 166.1 | 352.1 / 170.1 | 90.7 / 138.5 | 378.5 / 135.6 |
| at t=300 | 111.3 / 179.7 (-38%) | 415.6 / 191.5 | 156.7 / 165.7 (-5.4%) | 462.8 / 115.3 |
| `Arenas:` | 16 (64 automatic) | 64 (64 automatic) | 16 | 64 |

Under the mode the arena count reads 16 on 16 CPUs whatever the thread
count (`percpu_arena:percpu` routes by `mm_cid`), and the idle soak's
resident set falls from 2.2..3 x jemalloc's to below it: "RSS bounded by
cores, not threads" is what the mode delivers. The 16-worker soak's
resident set halves (397 -> 187 MiB, 413 -> 208) but still ends 10% (C4A)
and 48% (C4) above jemalloc's at t=600, with a much lower floor after each
full GC (67 vs 92 MiB on the C4A). A reading caveat in both idle tables:
the jemalloc `after the last full GC (final)` row (127.0 / 119.5 MiB) has
no sample behind it in the RSS-over-time table (`0.0 | - | -` at t=300 for
jemalloc): the soak's final sample of the jemalloc run is missing in both
idle runs today and the `peak` and `t=300` rows are the ones to read
(`bench/ruby-soak.sh`, lane D's own tool; to look at, not a library
matter).

## 6. What the rows say for the decision (not the decision)

- Under `percpu_arena:percpu` the memory rows move the way task 8
  intended: larson64 RSS -43%, the Ruby idle soak from +117..+301% to
  -41..-4% against jemalloc, the 16-worker Ruby soak from +147% to
  +10..+48%, mstress x86_64 from +18.8% to +11.5% (still outside the 10%
  band; +9.7% on aarch64 either way).
- The time rows pay: larson +7..+30%, valkey1 +5% (C4A) / +34% (C4),
  xmalloc +12..+18%, glibc-simple +5..+17%, alloc-test +0..+6%, the Valkey
  soak's delete phase +3..+11% req/s and up to +30% memtier p99 behind
  jemalloc, erb_render's lead down from 14% to 8%. Lane A's attribution
  (the per-side section floor, the scheduled pop's cold path, the flush's
  copies) covers the larson and Valkey model rows; the soak's delete phase
  is one round.
- Correctness under the key: calloc-zero 100/100, container tests, Valkey
  suites (full on x86_64), Ruby `test-all` and both soaks clean; one real
  fault, `pac_pointers:true` with the mode on a FEAT_FPAC host (section
  1.2), for lane A before the mode can be called suites-green with every
  key; one suite/environment gap (section 1.1).
