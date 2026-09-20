# bench/

Benchmark protocol for challoc (docs/plan.md, "Benchmark protocol"). Everything
runs inside the dev container on the native architecture; emulated x86_64 is
never used for numbers.

    make bench            build the programs (once), cargo build --release,
                          run the protocol, write bench/results/<date>-<arch>.md
    make bench-build      just fetch and build the programs
    make quick [OLD=rev]  fast tier: ~2.5 min go/no-go for the working tree vs OLD
                          (PERF=1 adds per-op counters where perf exists)
    make ab VARIANTS=...  protocol tier A/B: the table results files are made of
    make baseline         cache jemalloc/glibc/mimalloc/tcmalloc protocol rows for this machine
    make scorecard        the fixed row set vs jemalloc, one line appended to
                          bench/results/scorecard-<arch>.md; RENDER=1 prints the trend
    make valkey-soak      Valkey soak under challoc and its bundled jemalloc
                          (Phase 1 task 8); needs `make valkey STAGE=build`
    make soak-quick       challoc-only 600 s soak vs the cached jemalloc soak
    make soak-baseline    the 1200 s jemalloc soak that soak-quick compares against
    make ruby-bench       Ruby 3.4 rows under challoc, jemalloc, glibc, mimalloc and
                          tcmalloc (Phase 1.5 task 4b); needs both `make ruby STAGE=build` builds
    make python-bench     Python 3.13 rows (Phase 2.5 task 1): four stdlib workloads, each
                          with PYTHONMALLOC unset and =malloc, under the same five allocators
    make node-bench       Node.js 22 rows (Phase 3 task 5 (c)): node -e 0 startup, a JSON
                          churn process and an http server under ab for DURATION s per allocator
    make jvm-bench        JVM rows (Phase 3 task 5 (a)): `java -version` startup and a
                          200-thread com.sun.net.httpserver server under ab for 5 minutes per
                          allocator with NMT (allocator overhead = Rss - NMT committed), plus
                          glibc with MALLOC_ARENA_MAX=2 and glibc with the JVM's periodic
                          malloc_trim (-XX:TrimNativeHeapInterval) as the sixth and seventh variant
    make jvm-soak         the JVM soak that can separate the allocators: a 16..400-thread pool,
                          log-uniform 256 B..1 MiB direct buffers, 60 s windows alternating 256
                          and 8 connections, 20 minutes per allocator, Rss series and slope
    make postgres-bench   PostgreSQL 17 rows (Phase 3 task 5b): pgbench select-only at 100 and
                          500 clients, TPC-B, connection setup; RSS over every postgres process

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
of `larson` 1 and 10 threads, `xmalloc-test -w 10 -t 5 -s 64`, `alloc-test 1`,
the Valkey model (`valkey1`, below, 5 s: `VALKEY_MODEL_SECS`) and the churn
probe (`bench/probes/churn.c`, 1 KiB class, 8 s) under old, new and
jemalloc, and prints one table:

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
a criterion. Knobs: `QUICK_BENCHES`, `CHURN_SECS`, `CHURN_CLASS`, `NEW=rev`,
`VALKEY_MODEL_SECS`.

`PERF=1` appends a per-operation counter table for `PERF_BENCHES` (`larson1
valkey1`) under old, new and jemalloc: cycles, instructions, L1D misses and
dTLB misses per operation from one `perf stat -x,` run each, divided by the
operation count the program prints (`bench/lib.sh ops_of`). It needs `perf`
in the container, which Wolfi does not have, so on the Mac it prints a note
and on the VMs the same table comes from `challoc-bench quick-perf <vm>`:
that syncs, runs `make quick QUICK_BUILD_ONLY=1` (builds only), then
`bench/gce/quick-perf.sh` on the VM host starts each row detached in the
container (`sleep 1; exec ...`), attaches `sudo perf stat --inherit -p` to
the container's init pid before the exec, waits, kills perf's sleep child,
and prints the table (raw files under `~/logs/quick-perf`). For `valkey1`
the per-operation figures are over the whole process (the fill and both
phases, divided by the two phases' operation count), so they average the
full-heap SETs with the delete phase's cheap misses; the row's score stays
the write phase. The counters are for
attribution (which mechanism a change moved), never for the gate.

**Target-shaped rows.** The larson/xmalloc/alloc-test rows have working
sets of a few MiB and cannot see what Valkey does to the allocator (a 550
MiB heap whose slab descriptors miss cache on every free of a random key,
`bench/results/2026-09-18-aarch64-gce-valkey-throughput.md`), so
`bench/lib.sh` has rows shaped like the targets:

- `valkey1` and `valkey1-free`: `bench/probes/valkey_model.c`, a
  single-thread model of the soak's server. 850k keys (`VALKEY_MODEL_N`),
  each a 40-byte entry, a 24-byte key, a 16-byte object and a value drawn
  uniformly from 16..1024 bytes plus a 5-byte sds header (about 500 MiB
  live, quantum8-like sizes across the classes Valkey uses); an initial
  fill, then `VALKEY_MODEL_SECS/2` seconds of SET over random keys
  (overwrite: allocate the new value and object, free the old pair; the
  heap stays full) and `VALKEY_MODEL_SECS/2` seconds of DEL:SET 4:1 over
  random keys (a DEL of a missing key is a miss, as memtier's are; the key
  count drains toward 20% as in the soak's delete phase). `valkey1` frees
  through `sdallocx(ptr, size, 0)` when the loaded allocator exports it
  (weak reference: challoc and jemalloc do, glibc, mimalloc and tcmalloc fall back to
  `free` and the log says so), `valkey1-free` forces plain `free`
  (`-f free`; `-f sdallocx` refuses to run without the symbol), and
  `valkey1-usable` (`-u`) precedes every free with a `malloc_usable_size`
  query of the pointer, which is what Valkey's unsized `zfree(ptr)` does
  (`zmalloc_size` for the accounting, then `je_sdallocx`); it is the row
  for the usable-size query (plan Phase 1.5 task 14), not a gate row. `valkey1-write` (`-w SECS -d 0`) is the write phase alone, so a per-operation counter from `quick-perf` is per SET (the instruction budget of plan Phase 2.5 task 4c; it is not in the `quick` set). Scored by
  the write phase's M ops/s (the phase whose gap the soak shows widest and
  the one that reproduces the large-heap free miss); the delete phase's
  rate, the RSS after it and, with a `mallctl`-exporting allocator,
  allocated/active/resident at each phase end are in the log (`-s`). RSS in
  the tables is the run's peak. Run it by hand:
  `LD_PRELOAD=target/release/libchalloc.so target/bench-bin/valkey_model -n 100000 -t 3 -s`.
- `redis`: `run.sh`'s recipe as a `lib.sh` row (server under `LD_PRELOAD`,
  `redis-benchmark -r 1000000 -n 1000000 -P 16 lpush/lrange`, k req/s, RSS
  is the server's), so `ab` and the scorecard can run it.
- `ruby-churn`: `bench/ruby/gc_churn.rb 10 10` on the `--without-jemalloc`
  Ruby build (`target/ruby/install-glibc`, installed on first use from
  `make ruby STAGE=build ALLOC=glibc`'s build directory), the same binary
  under every allocator with `LD_PRELOAD` only; wall seconds. Skipped with
  a note when that build does not exist (the VMs have it, the Mac usually
  not). `ruby-bench.sh` remains the full Ruby protocol.

**Scorecard: `bench/scorecard.sh`, `make scorecard`, `challoc-bench
scorecard <vm>`.** The trend record. One run measures the fixed row set
`larson1 larson10 larson16 xmalloc alloc-test1 alloc-testN valkey1
valkey1-free cfrac espresso mstress glibc-simple redis` under challoc
(`REV`, default the working tree) and jemalloc through `ab.sh` (`ROUNDS` 3
timed rounds plus warm-up, interleaved; `BASELINES=cached` allowed, with
its sanity round, when the cache covers every row: `make baseline
BENCHES="<the rows>"`), and appends one line for (date, commit) to
`bench/results/scorecard-<arch>.md`: every row as a signed percentage
against jemalloc (positive = challoc worse), the RSS ratio challoc/jemalloc
for `larson10` and `valkey1`, and a soak column group (write and delete
throughput, peak RSS, RSS after settle, each challoc vs jemalloc; the worse
of the two generators' rows per phase) read from the most recent Valkey
soak summary for the same commit on that architecture under
`bench/results/` or `target/valkey/soak-quick/`, "-" when there is none. A
line for the same (date, commit) is replaced. About 8 minutes on a VM (6
with the cache). `bench/scorecard.sh --render [aarch64|x86_64]` prints the
table(s) sorted by date and works on the host. The two files were seeded
from the GCE results files (the `(from ...)` note names the source; the
2026-09-16 Mac numbers are not in them). After landing a change, run the
scorecard on both VMs and commit the appended lines with the PROGRESS
commit (`scripts/ralph-prompt.md`). Since the lanes (CLAUDE.md,
Environment) only the coordinator does this, from `main` on the reference
pair (`CHALLOC_GCE_PREFIX` unset), once per merge batch; a lane iteration
judges its change with `quick`, `quick-perf` and `ab` on its own VMs
(`CHALLOC_GCE_PREFIX=challoc-<lane>`), writes those tables to
`bench/results/<date>-<arch>-<lane>-<task>*.md`, and never appends to the
two scorecard files.

**Noise.** `xmalloc-test -w 10` spreads about 20% between rounds of the
same binary on aarch64 (`2026-09-17-aarch64-gce-slab-geometry.md` run 2).
`PIN=1` in `run_row` (any tier; `PIN=<cpulist>` for an explicit list) runs
the row under `bench/pin.c` (a `taskset` stand-in: Wolfi has none), pinning
the process to CPUs 0..threads-1 of the row (`threads_of`; a
single-threaded row goes to CPU 1). Default off. This is an experiment for
the next VM session to evaluate against unpinned rounds of the same
binary; nothing here claims it reduces the spread, and no results file
uses it yet. The soak has its own artifact: memtier's default fixed-seed
key stream replays the same keys every run and covers the keyspace
partially, which pins about 2200 values per class through the delete phase
(`2026-09-17-aarch64-gce-compacting-cache-soak3.md`), so the post-delete
RSS row partly measures slab geometry under pinned objects.
`KEY_PATTERN=uniform` in `valkey-soak.sh` gives memtier `--randomize`
(time-seeded) with random key patterns in both phases; the default (`stock`)
is unchanged and remains the acceptance protocol, and the summary header
says which ran.

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
rows under jemalloc, glibc, mimalloc and tcmalloc once and stores the table under
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
  (`bench_args`, including `valkey1`, `valkey1-free`, `redis`,
  `ruby-churn`), running and scoring one row (`run_row`, `run_redis`, the
  `PIN` experiment), operation counts for per-op counters (`ops_of`), the
  churn probe (`run_churn`), probes compiled on demand (`ensure_probe`),
  building a variant from a rev or the working tree with its own
  `CARGO_TARGET_DIR` and remapped paths (`build_lib`), sorted lists,
  medians, overlap and percentage helpers, the baseline file name and row
  parser, and the bench lock shared with `run.sh`.
- `quick.sh`, `ab.sh`, `baseline.sh`, `scorecard.sh`: the two tiers, the
  cache and the trend record, above.
- `pin.c`: `sched_setaffinity` to a CPU list, then exec (`PIN=1`).

- `build.sh`: clones pinned mimalloc-bench, redis 6.2.7 and mimalloc, builds
  `cfrac espresso larson mstress xmalloc-test alloc-test glibc-simple`,
  `redis-server`/`redis-benchmark`/`redis-cli` (`MALLOC=libc`) and
  `libmimalloc.so` into `target/bench-bin/`, plus the two helpers below.
- `rt.c`: `/usr/bin/time -v` replacement (Wolfi has no GNU time): wall time,
  user, sys, max RSS and exit code from `wait4`, appended to `$RT_OUT`.
- `sampler.c`: `perf record` replacement (Wolfi has no perf): a SIGPROF PC
  sampler preloaded after the allocator; writes `count module offset` lines.
- `run.sh`: one warm-up plus `RUNS` (5) timed runs per benchmark and
  allocator (`challoc jemalloc glibc mimalloc tcmalloc`), medians and raw runs into the
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
  and `SAVE_BASELINE=1` are the quick-soak cache (above), `KEY_PATTERN=uniform`
  the time-seeded memtier key stream ("Noise", above; not the acceptance
  protocol).
- `ruby-bench.sh`, `ruby/gc_churn.rb`: the Ruby rows (docs/plan.md Phase 1.5
  task 4b). The `--with-jemalloc` Ruby from `contrib/ruby/build.sh` runs
  under challoc's `libjemalloc.so.2` stage (re-staged from the working tree)
  and, with `LD_PRELOAD=/usr/lib/libjemalloc.so.2`, under Wolfi's jemalloc
  (same soname, so the loader binds `DT_NEEDED` to the preloaded copy; the
  script checks `/proc/self/maps` for every allocator); the
  `--without-jemalloc` build gives the glibc row and, with `LD_PRELOAD`, the
  mimalloc and tcmalloc rows. Rows are one process each: `ruby -e 1` startup,
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
- `python-bench.sh`, `probes/python/*_bench.py`: the Python rows (docs/plan.md
  Phase 2.5 task 1). Wolfi's `python-3.13` from the image runs four fixed
  stdlib workloads, `json` (json.dumps/loads of a 20000-record structure),
  `dict_list` (a ring of live dicts and lists replaced 30000 times), `regex`
  (four patterns over 3.5 MB of text) and `object_graph` (a 150000-instance
  tree, BFS, detach and rebuild), each in two modes: `default` (PYTHONMALLOC
  unset, pymalloc serves objects under 512 bytes and the allocator sees the
  rest) and `malloc` (`PYTHONMALLOC=malloc`, every object through the
  allocator). One process per row and round under `rt`, `ROUNDS` (5) plus
  warm-up, allocators interleaved (challoc built from `REV`, default the
  working tree, into `target/python/lib`; jemalloc, glibc, mimalloc and
  tcmalloc via `LD_PRELOAD`; `/proc/self/maps` checked once per allocator),
  written to `bench/results/<date>-<arch>-python.md` (`RESULT`, `ALLOCS`,
  `WORKLOADS`, `MODES`, `HOST_KIND`). pyperformance installs in the container
  but its pyperf worker protocol and hour-long standard set do not fit this
  shape, so it is not used (the script's header says why). The `json`
  workload is also the scorecard's `python1` (default) and `python1-malloc`
  rows (`bench/lib.sh`).
- `node-bench.sh`, `node/server.js`, `node/json_churn.js`: the Node.js rows
  (docs/plan.md Phase 3 task 5 (c)). Wolfi's `nodejs-22` from the image, the
  same binary under every allocator via `LD_PRELOAD` (challoc built from
  `REV`, default the working tree, into `target/node/lib`; `/proc/self/maps`
  checked once per allocator). Three rows: `startup` (`node -e 0`, wall
  time and max RSS from `rt`), `json_churn` (`node/json_churn.js`:
  `JSON.parse(JSON.stringify(obj))` over a 177 KiB object (180906 bytes;
  the plan said about 100 KiB, the results header records the size) 2000
  times in one process, wall time and max RSS, the Python rows' shape; both with
  `ROUNDS` (5) plus warm-up, allocators interleaved, sorted wall times and
  median RSS), and `server` (`node/server.js`, plain `http`, no npm
  dependencies, under `node --max-old-space-size=256` so the V8 heap is
  bounded and native memory is what differs: `GET /json` builds and
  stringifies a small object per request, `GET /blob` streams a 1 MiB body
  from a per-request `Buffer.allocUnsafe(1 << 20)`, filled and written in
  64 KiB chunks, so 1 MiB ArrayBuffer backing stores go through `malloc`
  and `free` at the request rate). The server row runs once per allocator
  for `DURATION` (300) s under `ab` from Wolfi's `apache2-utils` (Wolfi has
  no `wrk` or `oha`; two `ab` processes at once, `CONNS`/2 = 32 keep-alive
  connections on each endpoint; `-t` before `-n`, because `ab -t` resets
  `-n` to 50000), after a short warm-up; it reports each endpoint's req/s
  and p99 from `ab`, and the server's RSS as `/proc/PID/smaps_rollup` Rss
  sampled every `SAMPLE` (5) s during the load (peak and final sample),
  `VmHWM` at the end, and the end-of-load RSS summed by mapping name from
  `/proc/PID/smaps` (challoc's regions are named `challoc:classes`,
  `challoc:huge`, `challoc:meta`, so a gap can be placed). Written to
  `bench/results/<date>-<arch>-node.md` (`RESULT`, `ALLOCS`, `ROWS`,
  `DURATION`, `CONNS`, `HOST_KIND`).
- `jvm-bench.sh`, `jvm/HttpBench.java`: the JVM rows (docs/plan.md Phase 3
  task 5 (a)). Wolfi's `openjdk-21` from the image (under
  `/usr/lib/jvm/java-21-openjdk`, not on PATH; the script finds the newest
  JDK there or takes `JAVA_HOME`). Two rows: `startup`, `java -version` one
  process per round under `rt` (`ROUNDS` (5) plus warm-up, six variants
  interleaved, sorted wall times, median max RSS); and `server`,
  `HttpBench.java` (a plain JDK program, `javac`'d once into
  `target/jvm/classes`) on `com.sun.net.httpserver` with a fixed 200-thread
  executor (`THREADS`) and `-Xms512m -Xmx512m -XX:+AlwaysPreTouch`
  (`HEAP`; the Java heap is constant and resident from the start, so RSS
  differs between allocators by native memory only), served once per
  allocator for `DURATION` (300) s to two `ab -k -c 32` processes
  (`CONNECTIONS` (64) total; Wolfi's `apache2-utils`, since `wrk` and `oha`
  are not in Wolfi), one on `/json` (a ~300-byte body built per request) and
  one on `/blob` (64 KiB copied out of a per-request
  `ByteBuffer.allocateDirect`, one `malloc(65536)` in the JVM per request,
  freed in bursts when the buffer's Cleaner runs after a GC), after `WARMUP`
  (15) s of discarded load. The server's `/proc/<pid>/smaps_rollup` Rss and
  `/proc/<pid>/status` VmHWM are sampled every `INTERVAL` (5) s (idle before
  the load, peak, final), `ab` gives req/s and p99 per endpoint, `-Xlog:gc`
  the GC count, and the final `/proc/<pid>/smaps` Rss by mapping name
  (`[heap]`, challoc's named regions, unnamed anonymous, file-backed) plus
  the allocator's own stats print at exit (`MALLOC_CONF=stats_print:true`,
  `MIMALLOC_SHOW_STATS=1`, server row only) go to `OUT` for attribution.
  The server JVM runs with `-XX:NativeMemoryTracking=summary` (`NMT`; `off`
  disables it) and `jcmd <pid> VM.native_memory summary` is taken at the
  peak-Rss sample (a sample that sets a new peak asks for a snapshot, taken
  at the first sample at least `NMT_GAP` (30) s after the previous one, so
  an allocator whose Rss plateaus early still gets its snapshot on the
  plateau) and at the end: the results carry NMT's total committed,
  its malloc total and the Java Heap, Class, Thread, Code, GC, Compiler,
  Internal and Other categories, and "allocator overhead" = Rss minus NMT
  committed at the same instant, the number an operator fights (NMT's own
  cost, a 16-byte header per malloc plus its tables, is the same for every
  allocator and is reported; the header does move `malloc(65536)` across a
  power-of-two class boundary in every size-class allocator, see the
  results files' reading). Variants (`ALLOCS`): challoc (`REV`, default the
  working tree, built into `target/jvm/lib`), glibc, `glibc-arena2` (glibc
  with `MALLOC_ARENA_MAX=2`, the fleet's usual mitigation for the per-thread
  arenas), `glibc-trim` (glibc with the JVM's own
  `-XX:+UnlockExperimentalVMOptions -XX:TrimNativeHeapInterval=5000`,
  `TRIM_MS`: a `malloc_trim(0)` from the JVM's service thread every 5 s,
  what a well-tuned Java image ships today; the script checks that the JDK
  accepts the flag and refuses the variant otherwise), jemalloc, mimalloc,
  tcmalloc, every one's `/proc/self/maps` checked once. Written to
  `bench/results/<date>-<arch>-jvm.md` (`RESULT`, `ROWS`, `HOST_KIND`).
  `make jvm-soak` (`ROWS=soak`, alone) is the shape that can separate the
  allocators where the 5-minute row with one buffer size cannot (glibc's
  arenas never fragment there and the JVM's biggest native consumers are
  mmapped by the JVM itself): `HttpBench soak`, a `ThreadPoolExecutor` of
  `SOAK_CORE` (16) core and `SOAK_MAX` (400) maximum threads retired after
  `SOAK_KEEPALIVE` (2) s idle, `/blob` bodies drawn per request log-uniform
  between `SOAK_MIN` (256) and `SOAK_MAXB` (1048576) bytes (direct
  ByteBuffers of mixed sizes, no single size class dominates), the two
  generators alternating `SOAK_WINDOW` (60) s windows at `SOAK_HIGH` (256)
  and `SOAK_LOW` (8) total connections (threads are created and retired all
  run long: what makes glibc's arenas multiply), `SOAK_DURATION` (1200) s
  per allocator under challoc, glibc, glibc-arena2, glibc-trim and jemalloc
  (`ALLOCS`); Rss every `INTERVAL` s into
  `bench/results/<date>-<arch>-jvm-soak-<allocator>.csv` beside the results
  file (`-<allocator>-windows.csv` has ab's req/s and p99 per endpoint, the
  server's CPU per request, Rss, threads and the pool's size per window),
  summarised as peak, final, the mean and the least-squares slope over the
  last `SOAK_TAIL` (600) s in MiB per minute (growth after warm-up is the
  fragmentation signal), NMT at the end. Written to
  `bench/results/<date>-<arch>-jvm-soak.md`.
- `postgres-bench.sh`, `probes/pg-rss-sampler.sh`: the PostgreSQL rows
  (docs/plan.md Phase 3 task 5 (b)). Wolfi's `postgresql-17` from the image:
  the script creates a non-root OS user (`pgb`; postgres refuses to run as
  the container's root), runs `initdb` into `target/postgres/data`
  (`--locale=C`, trust auth on a unix socket), loads `pgbench -i -s 50` once
  (the data files do not depend on the server's allocator) and, per
  allocator and round, starts the server with `pg_ctl start` as that user
  with the allocator in its environment (`LD_PRELOAD`; challoc built from
  `REV`, default the working tree, into `target/postgres/lib`; jemalloc,
  mimalloc, tcmalloc by path; glibc none), so the postmaster and every
  backend and auxiliary process it forks inherit it; the postmaster's and
  one live backend's `/proc/<pid>/maps` are checked once per allocator.
  Server options go on the command line (`shared_buffers` 1 GB so the
  scale-50 set is served from shared memory, `max_connections` 600,
  `huge_pages=off`, `synchronous_commit=off`, no checkpoint inside a row).
  Rows are one pgbench run each (pgbench, the client, runs as root on glibc
  malloc, the same binary for every allocator): `select100` (`-S -c 100 -j
  16 -T 60`), `tpcb100` (TPC-B, `-c 100 -j 16 -T 60`), `select500` (`-S -c
  500 -j 16 -T 60`) and `connect20` (`-C -c 20 -j 4 -T 30`, a new
  connection per transaction: the fork and backend start-up row). Scores:
  pgbench's tps and latency average, p99 from `-l --sampling-rate=0.05`,
  connect20's "average connection time", and RSS from the sidecar
  `pg-rss-sampler.sh`, which runs as the postgres user (root in the
  container cannot read another uid's `smaps_rollup`) and sums
  `smaps_rollup` Pss and Pss_Anon over every postgres process every
  `INTERVAL` (5) s: peak and final of the run, and the per-backend median
  at the final sample (the 100- and 500-connection points). `ROUNDS` (5)
  plus warm-up, allocators interleaved (one server start per allocator per
  round), sorted tps and medians written to
  `bench/results/<date>-<arch>-postgres.md` (`RESULT`, `ALLOCS`, `ROWS`,
  `SCALE`, `DURATION`, `CONN_DURATION`, `CLIENTS_A`, `CLIENTS_B`,
  `HOST_KIND`; `REPORT_ONLY=1` re-renders the file from a finished run's
  raw samples and scores under `OUT`). A protocol run is about two hours
  per architecture; a sampler pass over 500 backends takes tens of
  seconds on a saturated host, so the 500-client row gets a few passes
  per run (the table says how many).
- `four-way.sh`: assembles the comparison matrix of Phase 2.5 task 1
  (`bench/results/<date>-<arch>-gce-four-way.md`) from one VM's same-run
  outputs: an `ab.sh` result (`AB=`, the Phase 0 set and the Valkey model rows
  under `challoc jemalloc glibc mimalloc tcmalloc`), a `ruby-bench.sh` result
  (`RUBY=`), a `python-bench.sh` result (`PYTHON=`) and the container-test
  logs (`CGROUP=`); one table with every allocator's score and peak RSS and
  challoc against each of the other four (time / RSS, positive = challoc
  worse), then the list of rows more than 10% behind, under which the
  attribution is written by hand. Reads only; pure sh + awk, runs on the host.
- `probes/`: `churn.c` (the post-delete steady-state model `quick` runs;
  `churn-run.sh` runs it by hand with the cache knobs, `CHURN_SECS`),
  `valkey_model.c` (the Valkey model behind the `valkey1` rows, above),
  `binsample.c` (per-second bin row sidecar), `soak-occupancy.sh` (now a
  wrapper over `valkey-soak.sh OCCUPANCY=1`), `valkey-perf-sidecar.sh` (root, on
  the GCE VM host beside a detached soak: `perf stat -I 1000` for each
  `valkey-server`'s life, 60 s `perf record` windows in the write and delete
  phases, `INFO commandstats` and per-thread CPU snapshots; task 4d).
- `results/`: one file per run, named `<date>-<arch>.md`, with the commit
  hash, toolchain versions, kernel and CPU in the header; plus the two
  scorecards `scorecard-aarch64.md` and `scorecard-x86_64.md`, one line per
  (date, commit), appended by `scorecard.sh` (its column set grew once, on
  2026-09-18, by `python1` and `python1-malloc`; the script pads older lines
  with `-` there when it renders or appends).

Benchmark programs and their parameters follow mimalloc-bench's `bench.sh`
(`larson 5 8 1000 5000 100 4141 N`, `mstress N 50 25`, `xmalloc-test -w N -t 5
-s 64`, `alloc-test 1` and `alloc-test min(N,16)`, redis `-r 1000000 -P 16
lpush/lrange`) with N = `nproc`, except redis uses `-n 1000000` instead of
`100000` so a run lasts more than a second.

## Measurement hygiene from the kernel table (Phase 2.5 task 17)

Three opt-in readings the 2026-09-19 kernel table made possible, all in the
harness, none in `src/`:

- **`memory.peak` per phase.** From 6.12 a write to the cgroup's
  `memory.peak` resets the high-water mark, but only for the file
  description that wrote it, so `bench/probes/cgpeak.c` holds one descriptor
  for a run and is driven by the soak's phase file (`bench/lib.sh
  cgroup_peak_*`). `make valkey-soak CGROUP_PEAK=1` adds three rows to the
  summary, "cgroup memory.peak MiB, write / delete / settle phase", beside
  the server's VmHWM; `make ruby-soak CGROUP_PEAK=1` adds "start..compact"
  and "compact..end" (the reset lives in `bench/ruby/soak.rb`, which holds
  its own descriptor); `make percpu-campaign` records the largest
  `memory.peak` of each step's container cgroups from the host side (fresh
  cgroups per `docker run`, no reset needed) and passes `CGROUP_PEAK=1` to
  its soak steps. `CGROUP_PEAK=1` gives the container `--cgroupns=host` and
  a read-write `/sys/fs/cgroup`: Docker mounts the tree read-only and the
  nsdelegate boundary refuses the write from inside the namespace
  otherwise. Without it, or on a kernel before 6.12 (EINVAL), the rows read
  `-` and the header says why; VmHWM stays the criterion's figure. The
  cgroup's peak counts every process of the container (server, load
  generators, harness), which is the point of the column.
- **THP counters around a row.** `CHALLOC_THP_COUNTERS=1` makes `quick`,
  `ab` and `challoc-bench quick-perf` snapshot every `thp_*` line of
  `/proc/vmstat` and every `hugepages-<size>kB/stats/*` counter before and
  after each run and print the deltas (`anon_fault_alloc`,
  `anon_fault_fallback`, `split`, `swpout`, ... per size) as a section after
  the table, so task 9's dTLB column has the folio counts next to it. The
  counters are host-global: a quiet host is assumed.
- **Guard slabs through `PAGEMAP_SCAN`.** `make test-pagemap-guard` (in
  `make test`) runs `tests/c/pagemap_guard.c` both ways: under the default
  it finds the guard slabs of four classes as the gaps no allocation came
  from and asks `ioctl(PAGEMAP_SCAN)` about them without touching them
  (6.16+: `PAGE_IS_GUARD` on the guard page and nowhere else; older
  kernels: the fallback the program names), under `guards:false` it finds no
  gap and no guard page.

