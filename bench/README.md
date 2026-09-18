# bench/

Benchmark protocol for challoc (docs/plan.md, "Benchmark protocol"). Everything
runs inside the dev container on the native architecture; emulated x86_64 is
never used for numbers.

    make bench            build the programs (once), cargo build --release,
                          run the protocol, write bench/results/<date>-<arch>.md
    make bench-build      just fetch and build the programs
    make quick [OLD=rev]  fast tier: ~2 min go/no-go for the working tree vs OLD
    make ab VARIANTS=...  protocol tier A/B: the table results files are made of
    make baseline         cache jemalloc/glibc/mimalloc protocol rows for this machine
    make valkey-soak      Valkey soak under challoc and its bundled jemalloc
                          (Phase 1 task 8); needs `make valkey STAGE=build`
    make soak-quick       challoc-only 600 s soak vs the cached jemalloc soak
    make soak-baseline    the 1200 s jemalloc soak that soak-quick compares against
    make ruby-bench       Ruby 3.4 rows under challoc, jemalloc, glibc and mimalloc
                          (Phase 1.5 task 4b); needs both `make ruby STAGE=build` builds

## Two tiers

An implementation-loop iteration needs a cheap answer to "is this change
worth measuring properly?" long before it needs a number that can go into a
results file. The harness therefore has two tiers, and the rule is: always
the fast tier first, the protocol tier only for a variant that passed it.

**Fast tier: `bench/quick.sh`, `make quick`, `challoc-bench quick <vm>`.**
About two minutes on a reference VM. It builds the working tree into
`target/quick/new` and `OLD` (default `HEAD`, so an uncommitted change is
measured against its own base; any git rev works, `OLD=HEAD~1` for a
committed one) via `git archive` into `target/quick/old`, checks that both
compiled and that the two `.so` files differ exactly when the sources do
(paths are remapped, so identical sources give an identical library and a
comment-only change is reported as "byte-identical"), then runs one round
of `larson` 1 and 10 threads, `xmalloc-test -w 10 -t 5 -s 64`, `alloc-test 1`
and the churn probe (`bench/probes/churn.c`, 1 KiB class, 8 s) under old, new
and jemalloc, and prints one table:

    | row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
    | larson1 | M ops/s | 44.5 (13.3 MiB) | 44.6 (13.4 MiB) | 45.1 (18.2 MiB) | -0.2% | +1.1% |
    ...
    | churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3461 (96.6%) | ... | new compacts (end occupancy >= 90%) | ... ops/s |

Positive percentages mean new did worse. The exit status is 1 when new is
more than `QUICK_TOLERANCE` (5) percent behind old on any row or the churn
probe under new ends below `CHURN_MIN_OCC` (90) percent occupancy (the plan's
"469 -> under 230 slabs" in the 64 KiB geometry; occupancy does not move with
slab size, the slab count does). A row that fails the gate on its single round is rerun for
`QUICK_CONFIRM_ROUNDS` (2) more rounds of old and new and judged on the
median, because one larson round spreads 5..9% on the reference VMs; rows
that pass are not rerun. One round is a signal, not a measurement:
a fail means abandon or rework, a pass means run the protocol tier. Numbers
from `quick` are never written to `bench/results/` and never compared with
a criterion. Knobs: `QUICK_BENCHES`, `CHURN_SECS`, `CHURN_CLASS`, `NEW=rev`.

**Protocol tier: `bench/ab.sh`, `make ab`, `challoc-bench ab <vm>`.** The
A/B that results files are written from, replacing the `.tmp-itNN-inner.sh`
scripts iterations used to rewrite. `VARIANTS` is a list of
`name=rev[:MALLOC_CONF]` and stock allocator names, for example

    make ab VARIANTS='old=HEAD~1 new=HEAD narenas16=HEAD~1:narenas:16 jemalloc'

Each rev is built once from `git archive` into `target/ab/<hash>` (`tree`
means the working tree, into `target/ab/tree`); two variants sharing a rev
share the build. `ROUNDS` (5) timed rounds plus one discarded warm-up, every
variant runs every bench within a round (interleaved, so drift hits all
alike), every run's score is listed sorted, RSS is `rt`'s median maxrss in
KiB, and the load average at start and end is in the header. `BENCHES`
defaults to `larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN`
(`bench/lib.sh` also knows `larsonN xmallocN cfrac espresso mstress
glibc-simple`). The output is the markdown shape of
`bench/results/*-gce-*.md` (a "Sorted scores" table and a "Medians" table
with percentages against the first variant), written to `RESULT=path` when
set (default `target/ab/result.md`) and printed. `ADAPTIVE=1` stops after 3
timed rounds when, on every bench, no two variants' sorted lists overlap;
otherwise it runs all `ROUNDS`. Acceptance runs use the defaults: 5 rounds,
every variant including jemalloc measured in the same run.

**Cached baselines.** `make baseline` (`bench/baseline.sh`) runs the protocol
rows under jemalloc, glibc and mimalloc once and stores the table under
`target/baselines/<arch>-<bench-build-hash>.md`, the hash covering the
benchmark program versions, kernel and CPU count (`target/` is per machine,
so a cache is only ever read on the machine that wrote it). `quick.sh` and
`ab.sh` take `BASELINES=cached` to fill the stock-allocator columns from
that file instead of rerunning them. Every cached run still executes one
jemalloc sanity round (`quick`: the `QUICK_SANITY` rows, default `larson1`;
`ab`: warm-up plus one round of every bench) and compares it with the cached
median: more than `BASELINE_TOLERANCE` (3) percent off on any bench declares
the cache stale, the table is still printed, and the exit status is 2; rerun
`make baseline`. Same-run comparison stays the default and is what
acceptance uses; the cache is for iteration runs on a warm VM.

**Quick soak.** `make soak-quick` runs `bench/valkey-soak.sh` for challoc
only (`DURATION=600 SETTLE=60`) with the occupancy sampler on
(`OCCUPANCY=1`: `MEMORY MALLOC-STATS` and the allocator lines of `INFO
memory` every `MS_INTERVAL` (20) seconds into
`RESULT_DIR/<date>-<arch>-valkey-soak-challoc-mstats/<t>.txt`) and compares
it with the cached jemalloc soak that `make soak-baseline` produced (the
full `ALLOCS=jemalloc DURATION=1200` protocol, stored as
`target/baselines/valkey-soak-jemalloc-<arch>.csv` plus `.runs.csv`,
`.vm.txt`, `.dbsize.txt`, `.version.txt`, `.meta`). The summary states that
the jemalloc figures are cached, their date, commit and duration, and that
with different phase lengths only the throughput, p99 and peak rows are like
for like. Results land under `target/valkey/soak-quick`, never in
`bench/results/`. The two-allocator 20-minute `make valkey-soak` is
unchanged and remains the acceptance run.

Files:

- `lib.sh`: what the tiers share: the protocol rows as command lines
  (`bench_args`), running and scoring one row (`run_row`), the churn probe
  (`run_churn`), building a variant from a rev or the working tree with its
  own `CARGO_TARGET_DIR` and remapped paths (`build_lib`), sorted lists,
  medians, overlap and percentage helpers, the baseline file name and row
  parser, and the bench lock shared with `run.sh`.
- `quick.sh`, `ab.sh`, `baseline.sh`: the two tiers and the cache, above.

- `build.sh`: clones pinned mimalloc-bench, redis 6.2.7 and mimalloc, builds
  `cfrac espresso larson mstress xmalloc-test alloc-test glibc-simple`,
  `redis-server`/`redis-benchmark`/`redis-cli` (`MALLOC=libc`) and
  `libmimalloc.so` into `target/bench-bin/`, plus the two helpers below.
- `rt.c`: `/usr/bin/time -v` replacement (Wolfi has no GNU time): wall time,
  user, sys, max RSS and exit code from `wait4`, appended to `$RT_OUT`.
- `sampler.c`: `perf record` replacement (Wolfi has no perf): a SIGPROF PC
  sampler preloaded after the allocator; writes `count module offset` lines.
- `run.sh`: one warm-up plus `RUNS` (5) timed runs per benchmark and
  allocator (`challoc jemalloc glibc mimalloc`), medians and raw runs into the
  results file. larson and xmalloc-test are fixed-duration, so they are scored
  by the throughput they print; redis by redis-benchmark's requests/second.
- `profile.sh`: larson under challoc with the sampler, attributed to symbols
  (`nm`) and instructions (`objdump`), flagging lock and atomic RMW
  instructions. Appended to the results file by `run.sh`.
- `valkey-soak.sh`: starts `valkey-server` from the challoc tree built by
  `contrib/valkey/build.sh` and from an unpatched clone of the same tag built
  with bundled jemalloc 5.3.0 (`target/valkey/src-<tag>-jemalloc`, built on
  first use), runs a write-heavy phase (valkey-benchmark `-t set`, then
  memtier_benchmark SET-only with 16-1024 byte values) and a delete-heavy phase
  (valkey-benchmark `del`, then memtier DEL:SET 4:1) of `DURATION/2` seconds
  each over a 500k-key keyspace, then `SETTLE` idle seconds. `INFO memory` and
  `INFO stats` are sampled every `INTERVAL` seconds into
  `results/<date>-<arch>-valkey-soak-<alloc>.csv` (used_memory, used_memory_rss,
  mem_fragmentation_ratio, allocator_frag_ratio, allocator_rss_ratio,
  active_defrag_hits, plus the throughput and client-side p99 of the most
  recent generator run); `results/<date>-<arch>-valkey-soak.md` compares
  throughput, p99, peak RSS (VmHWM) and post-settle RSS against jemalloc.
  Defaults are the Phase 1 acceptance protocol (20 minutes of load per
  allocator); `DURATION=120 SETTLE=30 CHUNK=10 VB_REQS=1000000
  RESULT_DIR=target/valkey/soak-results` is a five-minute smoke run.
  `OCCUPANCY=1` adds the per-class occupancy sidecar, `BASELINES=cached`
  and `SAVE_BASELINE=1` are the quick-soak cache (above).
- `ruby-bench.sh`, `ruby/gc_churn.rb`: the Ruby rows (docs/plan.md Phase 1.5
  task 4b). The `--with-jemalloc` Ruby from `contrib/ruby/build.sh` runs
  under challoc's `libjemalloc.so.2` stage (re-staged from the working tree)
  and, with `LD_PRELOAD=/usr/lib/libjemalloc.so.2`, under Wolfi's jemalloc
  (same soname, so the loader binds `DT_NEEDED` to the preloaded copy; the
  script checks `/proc/self/maps` for every allocator); the
  `--without-jemalloc` build gives the glibc row and, with `LD_PRELOAD`, the
  mimalloc row. Rows are one process each: `ruby -e 1` startup,
  `so_binary_trees`, `hash_aref_str`, `array_sample_100k__1k`,
  `array_sort_block`, `so_nbody`, `app_fib` and `erb_render.yml` from
  `ruby/benchmark` through `benchmark-driver --runner time` (cloned once into
  `target/ruby/benchmark-driver`; its `-e` executable is a wrapper that execs
  `rt` around the measured ruby), `gem list`, and `gc_churn.rb` (long-lived
  medium buffers, bursts of short-lived ones, ten forced full GCs; `GC.stat`
  counts and RSS before/after the forced GCs in their own table). `ROUNDS`
  (5) plus warm-up, allocators interleaved, sorted wall times and median
  RSS from `rt`, written to `bench/results/<date>-<arch>-ruby.md`
  (`RESULT`, `ALLOCS`, `ROWS`, `HOST_KIND`). Both builds are installed into
  their `target/ruby/install-<alloc>` prefixes on first use (the build
  directory's ruby has no stdlib path).
- `probes/`: `churn.c` (the post-delete steady-state model `quick` runs;
  `churn-run.sh` runs it by hand with the cache knobs, `CHURN_SECS`),
  `binsample.c` (per-second bin row sidecar), `soak-occupancy.sh` (now a
  wrapper over `valkey-soak.sh OCCUPANCY=1`).
- `results/`: one file per run, named `<date>-<arch>.md`, with the commit
  hash, toolchain versions, kernel and CPU in the header.

Benchmark programs and their parameters follow mimalloc-bench's `bench.sh`
(`larson 5 8 1000 5000 100 4141 N`, `mstress N 50 25`, `xmalloc-test -w N -t 5
-s 64`, `alloc-test 1` and `alloc-test min(N,16)`, redis `-r 1000000 -P 16
lpush/lrange`) with N = `nproc`, except redis uses `-n 1000000` instead of
`100000` so a run lasts more than a second.
