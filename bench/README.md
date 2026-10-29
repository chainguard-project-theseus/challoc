# bench/

Benchmark harness for challoc: **`bench/cb`**, one declarative runner
(`cb run` on this machine, `cb launch` on GCE cells) over the rows of
`bench/rows.toml`, the suites of `bench/suites/*.toml` and the cells of
`bench/cells/*.toml`, with paired, CI-backed verdicts.

    make quick [BASE=main] [HEAD=HEAD]           cb launch quick: the PR go/no-go on the gate cells (GCE)
    make bench-launch SUITE=<suite> CB_ARGS=...  any suite on GCE: full, apps, soak, aa, sidecar, exp-*
    make bench-quick | bench-aa | bench-smoke    the same suites on this machine (indicative only)
    make bench-local SUITE=<suite> CB_ARGS=...
    make bench-compare DIRS=... | bench-unit     host side: re-pool runs, unit tests
    make bench-image | bench-gc                  the bench image; delete one run's VMs
    make bench-build                             the bench programs into the target volume (local runs)
    python3 bench/cb {run,launch,gc,compare,report,fp,plan,rows,suites,cells,build,preflight,host} --help

Suites (`python3 bench/cb suites`):

    quick       head vs base on the gate cells: larson1/10, xmalloc, alloc-test1, valkey1(-free), churn1024
    full        every paired row, challoc vs jemalloc 5.4.0 (gate) and glibc (context): the general baseline
    apps        the application subset of full: RocksDB, nginx, compilers, ONNX Runtime, runtimes, PostgreSQL
    sidecar     the idle-sidecar and startup rows in a 512 MiB container
    soak        Valkey, Ruby and JVM soaks, head vs jemalloc, one variant per VM
    aa          base vs an identical base: calibration and the false-positive rate
    smoke       every quick row once, parse only (CI's bench-smoke); `--rows` smokes any row
    exp-ladder  the CPU ladder: challoc vs jemalloc per CPU family
    exp-floor   the kernel floor: `make test` and `make fuzz` per public kernel image
    exp-churn   #94: larson thread churn at 10/100/1000 rounds per thread with PMU counts (metal cells)
    exp-narenas, exp-slice   experiments (below)

Everything builds and runs inside the Wolfi bench image on the native
architecture; emulated x86_64 is never used for numbers. The workflow
for a change: `make quick` (about 6 minutes from cold) first; a change
that passes and is meant to help a workload gets that workload's suite
(`full`, `apps` or `soak`) and a per-operation attribution ("Attribution"
below).

## cb: the declarative runner

`bench/cb/` is a Python 3.11+ standard-library package that runs the rows
of `bench/rows.toml` as the suites of `bench/suites/*.toml` say, on the
cells of `bench/cells/*.toml`.

    make bench-quick [BASE=main] [HEAD=HEAD] [CB_ARGS='--blocks 4']
    make bench-aa    BASE=<rev>             # A/A: every row should come out `same`
    make bench-smoke [HEAD=<rev>]           # every quick row once, parse only
    make bench-local SUITE=<suite> CB_ARGS='--variants base,head,jemalloc --rows larson1'
    make bench-compare DIRS='target/results/cb/<run> ...'    # host side
    make bench-unit                                          # host side
    python3 bench/cb {run,compare,report,plan,rows,suites,cells,build,preflight,host} --help

What `cb run SUITE` does, in the dev container or on any Linux host:
builds every challoc variant from an archive of its rev into
`target/cb/build/<tree>` (keyed by tree hash, so an A/A builds once and a
rebuilt rev is a cache hit; paths are remapped, identical sources give an
identical `.so`, and the manifest flags different trees with identical
libraries), adds the stock comparators (`jemalloc`, `jemalloc53`,
`jemalloc54`, `glibc`, `tcmalloc`, `mimalloc`) as asked, captures the host (`host.json`: kernel, CPU and
microcode, THP per size and khugepaged, governor, caches, page size, glibc,
the image id, the challoc `features:` line under the probed library, the
PMU probe) and a comparability key, waits for a quiet host, then runs
blocks. In each block the rows are shuffled with a recorded seed and each
row runs every variant twice at mirror positions (ABBA for two, a Williams
sequence and its reverse for more), one fresh process per run under
`tools/rt` with a random 0..4 KiB environment pad; a sentinel
(`tools/sentinel.c`) opens every block. Samples are appended to
`samples.jsonl` as they finish. Output: `target/results/cb/<run>/`
(`plan.json manifest.json host.json samples.jsonl blocks.jsonl
summary.json report.md logs/`), readable on the host.

The verdict (`bench/cb/stats.py`): per block the paired log contrast
(positive = the candidate is worse), the estimate the mean over hosts of
each host's mean contrast, the Student-t CI on those k host means (df =
k - 1, so the host x variant interaction is in it however many blocks a
host ran; one host: the t interval on its blocks, flagged) at the suite's
per-look alpha (Pocock, 97.06% for two looks), and a verdict against the
row class's margin (1% single-thread, 2% multithread, 3% server, until 14
A/A estimates of that row on that cell calibrate it: `cb compare
--aa-history` takes one JSONL line per (run, cell, row)): `worse`, `better`, `same`,
`sub-threshold`, `inconclusive` or `broken`. The headline over the gate
rows (Holm across them): NEUTRAL, IMPROVEMENT, REGRESSION, INCONCLUSIVE,
BROKEN; the exit code is 0, 0, 1, 2, 1 (3 for an infrastructure failure).
Where counters exist the report adds the instructions/op delta and flags
`placement?` (time moved, instructions did not). Blocks whose sentinel is
3 MAD off or whose steal is above 1% are excluded and counted.

Local results are `indicative`: their comparability key names the host,
so they are never pooled with a GCE cell, and a laptop's numbers are for
the plumbing, not for a decision. Verdicts of record come from `cb launch`.

### cb launch: the GCE fan-out

    make quick [BASE=main] [HEAD=HEAD] [CB_ARGS=...]          # cb launch quick on the gate cells (= bench-launch-quick)
    make bench-launch SUITE=full CB_ARGS='--cells ref-amd64'  # one suite, one cell
    make bench-launch SUITE=aa BASE=<rev> CB_ARGS='--replicas 8 --looks 1'
    make bench-image                                      # rebuild + push the bench image, pin bench/internal-image.lock
    make bench-gc CB_ARGS='--run <run>'                   # delete one run's VMs
    python3 bench/cb launch quick --cells ref-amd64 --variants 'old=git:REV1@CONF,new=git:REV2@CONF'
    python3 bench/cb fp target/results/cb/<aa-run>/res/* --k 3   # FP rate of a 3-replicate run

`cb launch SUITE` runs on the host (Python 3.11+ and an authenticated
`gcloud`; the project, bucket and VM service account are in
`bench/cells/public.toml` `[defaults]`). It packs the suite's rows into
shards of about `shard_seconds` of runs (from each row's `est_s`), creates
one spot VM per (cell, shard, replicate) with `gcloud compute instances
bulk create` (`--max-run-duration` with DELETE, labels
`purpose=challoc-bench,cb-run=<run>`, at most `--max-vms`/`CB_MAX_VMS`
(32) of this launch alive at once and at most `CB_PROJECT_MAX_VCPUS`
(2000) vCPUs of cb VMs in the project, every launch's counted; the rest
wait for room, and are given up after 30 minutes without a create), and
polls. The quick suite on the two gate cells is 5 shards x 3 replicates x
2 cells = 30 VMs.

- **Bundle.** Each variant rev is resolved to (commit, tree) on the host;
  `git archive` of every tree, and of this checkout's tracked files as
  they are on disk (the harness: uncommitted changes included, untracked
  files not), goes to `gs://<bucket>/src/<tree>.<sha256>.tar.gz` once,
  and the table of VMs to `runs/<run>/launch.<sha256>.json`. A VM's
  metadata carries every object's sha256 and boot.sh refuses bytes that
  do not match.
- **The VM** (`bench/cb/boot.sh`, the startup script, on stock
  `ubuntu-2404-lts-{arm64,amd64}` or any cell image) reads its work from
  its instance metadata (harness and source objects, image, runtime,
  container profile, prep; no JSON parser needed on the host) and its
  entry of `launch.json` by its own name, works under `/var/cb`, stops
  the apt timers and snapd, runs the cell's prep steps, installs
  `docker.io` where the runtime is docker and it is missing while it
  fetches the sources, pulls the bench image by digest
  (`bench/internal-image.lock`; it builds `Dockerfile.dev`'s test stage and
  `bench/Dockerfile.cb` itself when the pull fails), fetches the perf
  bundle (below) to `/var/cb/perf`, runs `cb run --launch
  launch.json --vm <name>` in it (container profile `perfmon`:
  `CAP_PERFMON`, so the per-run counters work, and the cells request the
  PMU), PUTs the result files to `runs/<run>/res/<vm>/` whenever a block
  ends, then `done.json` and the marker `runs/<run>/done/<vm>.json`, and
  idles. The launcher deletes a VM as soon as its marker lands; a VM never
  powers off (a stopped VM would outlive its max-run-duration), so when
  the launcher is gone max-run-duration deletes it.
- **Failures.** A VM that vanishes (spot preemption deletes it) or stops
  without a marker is rescheduled under a new name, on demand after two
  spot failures; a create that fails in one zone tries the cell's next. A
  VM still running past its own max-run-duration (counted from its own
  creation, so later waves get their full time) is given up; a shard is
  lost only when no attempt of it finished.
- **Verdict.** The launcher downloads every VM's results to
  `target/results/cb/<run>/res/<vm>/`, pools them (one VM is one host in
  the statistics; comparability keys keep cells apart) and, when the
  headline is INCONCLUSIVE and the suite has two looks, adds
  `extend_replicas` VMs per cell for the unresolved gate rows only
  (inconclusive, or worse or better without surviving Holm) and pools
  again. Each VM's `logs.tgz` is fetched and unpacked into its `logs/`. A
  smoke suite has no comparison: its headline is PASS (exit 0) when every
  shard finished and every run scored, BROKEN (exit 1) otherwise. `summary.json`, `report.md` and `launch-summary.json` (wall clock
  by phase, VM-side medians, VM-minutes and a cost estimate) land in
  `target/results/cb/<run>/`. Exit codes as `cb run`; 3 when nothing came
  back or gcloud failed. Anything still labelled with the run is deleted
  before `cb launch` exits. `cb gc --run <run>` deletes one run's VMs;
  without `--run`, `cb gc` deletes only cb VMs no live launch can want
  (stopped ones, and running ones past their max-run-duration plus 10
  minutes), never another session's running launch.
- **Credentials.** The VMs run with the `storage-rw` scope only, as a
  service account that can read and write objects in the bench bucket and
  pull from the bench Artifact Registry repository. Under the docker
  runtime the benchmarked container cannot reach the metadata server
  (boot.sh adds a DOCKER-USER reject rule; `host.json` records
  `metadata_server`), so code under test never holds the VM's token; under
  podman (`--network=host`) it can. Inputs are hash-checked (above);
  results under `runs/` are still writable by every cb VM (per-object
  signed URLs or a write-restricted account would close that). The bucket
  deletes objects after 30 days.
- **perf.** Neither the bench image (Wolfi has no perf package) nor a
  Chainguard OS host has perf, so cb ships its own: `bench/cb/perf-build.sh`
  builds tools/perf from a pinned kernel.org release in a Wolfi container
  (libelf/libdw, libtraceevent, zstd, the jevents tables; its shared
  libraries beside it under a `$ORIGIN` rpath), `bench/cb/perf-upload.sh`
  puts it in the bucket under its hash and pins `perf-<arch> =
  "<object>=<sha256>"` in `bench/internal-image.lock`, and every VM gets it
  as the `cb-perf` metadata. boot.sh checks the hash, unpacks it to
  `/var/cb/perf` (perf for the host itself, glibc 2.44 on Chainguard OS)
  and mounts it at `/opt/perf` in the container, where `cb run` puts it on
  PATH; `host.json` records `perf` (its version) and `pmu_devices` (the
  perf event sources, e.g. `arm_spe_0`). A failed fetch costs perf, not
  the run (the per-run counters come from rt, not perf).
- **The image.** `bench/Dockerfile.cb` is Dockerfile.dev's `test` stage
  plus Wolfi's jemalloc (pinned to 5.3.1-r3) and tcmalloc-minimal, the
  prebuilt `bench/build.sh` programs (`/opt/cb/bench-bin`: mimalloc-bench,
  jemalloc 5.3.0 and 5.4.0, RocksDB, wrk2, z3, onnxruntime, ...),
  Fleetbench from a bazel stage of its own, the crate's dependencies and,
  last, the runtimes the driver and application rows need (Valkey's build
  dependencies and memtier, Ruby, openjdk-21, Node.js, PostgreSQL, nginx,
  ghostscript, fluent-bit, strace, `ab`); `bench/cb/image.sh`
  builds it per architecture, pushes it and rewrites `bench/internal-image.lock`.
  Rebuild it when `bench/build.sh`, the stock allocators or Dockerfile.dev
  change; the digest is in every sample's comparability key.
- **Measured (us-east4-a).** A VM is ready to
  run about 34 s after its startup script starts (boot 10-20 s before
  that) and builds both variants in about 20 s. Quick from cold: first
  look 4.6-7.5 min (median 5.6 over four launches), 8.5-12.7 min when
  the second look runs; about $0.9 per two-cell launch. A known
  regression (larson1 54.7 -> 31.5 M ops/s on ref-amd64) comes out
  REGRESSION in 5.0 min. An
  8-replicate A/A (80 VMs): no false `worse` or `better` at the class
  margins; resampled 3-replicate runs show 0.19% false rows per row and
  0/300 false headlines, but never NEUTRAL: on the C4, larson10 (within-VM
  sigma 3.0%) and valkey1-free (7.8%) do not resolve inside a 2% margin
  at 3 VMs x 3 blocks, so the A/A headline is INCONCLUSIVE and the second
  look runs. `cb fp` prints these numbers for any A/A run.

### Cells, container profiles, host prep

A cell (`bench/cells/*.toml`) is a machine shape, a host image family and
how the bench container runs there:

- `runtime`: `docker` (installed at boot where missing: the Ubuntu cells)
  or `podman` / `sudo-podman` (root podman with `--network=host`, for
  bootc hosts that ship podman, no docker and no nft).
- `bootstrap`: `startup-script` (boot.sh as the startup-script metadata;
  the guest agent runs it as root), the only one `cb launch` runs.
- `container`: the profile, `default` (the runtime's default seccomp),
  `privileged` (also mounts debugfs, for knobs) or `perfmon` (CAP_PERFMON,
  per-run counters), optionally as a table with `cpus` (a CFS quota),
  `cpuset` (pinned CPUs: more threads than CPUs) and `memory` (with swap
  off); `cpusN` is short for `perfmon` with `cpus = N`. A suite's `container`, or `cb launch --container
  default,cpuset=0-3`, overrides the cells'. The profile and its limits are
  part of the comparability key, so runs under different profiles are
  never pooled.
- `prep`: host steps boot.sh runs before the container starts: `grow-var`
  (grow /var's filesystem online, for images that ship it small),
  `thp:<file>=<value>` (`thp:enabled=always`, `thp:64kB=madvise`),
  `sysctl:<key>=<value>`, `modprobe:<module>` (`modprobe:arm_spe_pmu`, the
  SPE perf source; a failure is logged, not fatal), `kernel:<package>`
  (Ubuntu: install a kernel flavour with its initramfs, make it the GRUB
  default and reboot into it once). A suite's `prep` is added to every
  cell's.
- `expect`: facts the booted host must show, checked in the VM before any
  run: `kernel` (a regex on `uname -r`), `page_size`. A mismatch fails the
  VM as infrastructure, never as a measurement.
- `disk` (default `hyperdisk-balanced`, `pd-balanced` on the families
  without Hyperdisk: E2, N1, N2, ...), `boot_disk_gb`, `spot` (`never` for
  metal), `max_run`, `zones`, `pmu`.

The image family is only a request. `cb launch` resolves every family to
the image it points at when the launch starts (`gcloud compute images
describe-from-family`), boots every VM of the launch from that one image
and records family, image and the booted `uname -r` in `host.json` and the
report; a family that does not resolve makes its cell **PENDING** (listed
in the report and `launch-summary.json`, nothing created, not a failure;
when every cell is pending the exit code is 2).

**Internal overlay.** Cells on non-public images live in
`bench/cells/internal-*.toml`: loaded like any cell file, deleted before
publication, and `cb cells --lint` fails when one of an overlay's
`[lint] markers` appears in a public file. Suites name such cells only
through tags (`exp-slice` selects `tag:exp-slice`), so a public tree
without the overlay still parses. The overlay records its own host facts
and the bootstrap spike (startup scripts run on those images, 2026-10-01).

### Variants and host knobs

A variant is (library, `MALLOC_CONF`, host knobs):

    base | head | aa=base | x=git:REV | tree        challoc builds (tree: local only)
    jemalloc | glibc | tcmalloc | mimalloc          stock allocators
    jemalloc53 | jemalloc54                         jemalloc release tarballs (bench/build.sh)
    LABEL=SRC@CONF                                  one build, another MALLOC_CONF
    LABEL=SRC[@CONF]+knob:NAME=VALUE[+knob:...]     plus host knobs

`jemalloc` is Wolfi's package (5.3.1, pinned in the image so a rebuild
does not move it); `jemalloc53` and `jemalloc54` are the 5.3.0 and 5.4.0
release tarballs built with upstream's default configuration. 5.4.0 is the
general-purpose bar (`full`, `apps`).

Knobs (`bench/cb/knobs.py`): `thp`, `thp.defrag`, `thp.<N>kB` (mTHP
sizes), `khugepaged.<file>`, `sched.llc_balancing` and `sched.<file>`
(debugfs), `rseq.<file>` (debugfs, e.g. `slice_ext_nsec`),
`sysctl.<a.b.c>`. The executor writes a variant's knobs right before each
of its runs and puts back the host's value before a variant without them,
so knobs get the same ABBA pairing as libraries; the host's values are
restored at the end. Knobs need the `privileged` profile (`cb launch`
refuses otherwise). Example: `chl-llc=head+knob:sched.llc_balancing=1,
chl-nollc=head+knob:sched.llc_balancing=0`.

A row's `features` (`quantum8` for Valkey, `jemalloc-soname` for the
--with-jemalloc Ruby) builds every challoc variant a second time with
those cargo features; the run uses that build for the row.

### Drivers

Rows with `kind = "soak"` or `"driver"` run a script under
`bench/drivers/` once per run instead of one program:

    valkey-soak.sh   Valkey 9.1 under valkey-benchmark + memtier: write, delete, settle
    ruby-soak.sh     the Sidekiq-shaped Ruby process (drivers/ruby/soak.rb)
    jvm-bench.sh     JVM http server and the mixed-size soak (drivers/jvm/HttpBench.java)
    rocksdb.sh       db_bench fillrandom, readrandom, readwhilewriting on one fresh database
    nginx.sh         an nginx gzip proxy to an nginx backend under wrk2 at a fixed rate
    compile.sh       make -j of Lua 5.4, one cc1 over the sqlite amalgamation
    startup.sh       N exec-to-exit runs of a small program (execloop) and one under strace -c
    sidecar.sh       fluent-bit: an HTTP burst, then idle; the RSS it settles at
    node-server.sh   bench/node/server.js under two ab clients (/json, /blob)
    postgres.sh      PostgreSQL 17 under pgbench (select-only, TPC-B, connect per transaction)
    fleetbench.sh    a Fleetbench binary, the geometric mean of its benchmarks

The contract: cb passes the variant as `CB_ALLOC` (`challoc`, `jemalloc`,
`glibc`, ...), `CB_LIB` (its library, the row's `features` build),
`CB_LIB_SHA256`, `CB_CONF` (its MALLOC_CONF) and `CB_VARIANT`, a scratch
directory `CB_OUT`, `RESULT_JSONL`, plus the row's `env` (`{var}`
expanded; `--var valkey_soak_secs=60` shortens a soak). The driver runs
that one allocator and appends `{"schema": "challoc.bench.driver/v1",
"metrics": {"name": number, ...}}` to `RESULT_JSONL`. The row's `metric`
is its score; each entry of its `metrics` is compared as a row of its own
(`valkey-soak.rss_settle_mib`, class `soak-rss`, 2% margin). The drivers
check that the process maps exactly the variant's library.

- **Valkey, fixed runpath.** One Valkey build serves every challoc
  variant: the patched tree is linked with `-Wl,-rpath,/opt/challoc/lib
  -Wl,--enable-new-dtags` (a RUNPATH) and `/opt/challoc/lib/libchalloc.so.0`
  is pointed at the variant's quantum8 library before the server starts;
  the jemalloc variant is the same tag with its bundled jemalloc 5.3.0,
  whose clients (`valkey-cli`, `valkey-benchmark`) every variant uses.
  Trees are cloned at `VALKEY_TAG` (commit-checked) on first use.
- **Ruby.** A challoc variant built with `jemalloc-soname` is preloaded;
  its soname satisfies the binary's `DT_NEEDED libjemalloc.so.2`, so the
  staged library is never mapped. A missing Ruby build is made first
  (minutes).
- **Placement.** `placement = "one-variant-per-vm"` (the soaks): `cb
  launch` gives every (row, variant, replicate) a VM of its own, one
  block, no warm-up, a `max-run-duration` of twice the estimate; the
  comparison is unpaired (each VM's mean log score, Welch's t interval on
  the log scale, flagged `unpaired`). On one host (`cb run`) the variants
  run in ABBA order as usual.
- The three soak drivers also run by hand, outside cb (`make run
  CMD='sh bench/drivers/valkey-soak.sh'`, `ALLOCS=...`, a markdown summary;
  the variables are in each script's header); cb mode is selected by
  `CB_ALLOC`. The short drivers source `drivers/cb.sh` and run under cb
  only.

### Experiment suites

An experiment is a suite: cells (by tag), variants (MALLOC_CONF and knobs
on one build), a container profile, rows. `exp-slice` (audit D9) runs
`off=head@slice_ext:off` against `auto=head@slice_ext:auto` on the kernel
7.2 cell (which grants the rseq time slice extension, `slice_ext=on` in
the features line) and a 6.18 cell (which cannot, `slice_ext=no-slice_ctrl`:
the negative control, where every row must come out `same`), container
pinned to 4 CPUs, rows larson16, larson64, xmalloc, valkey1, valkey1-free.
The verdict was cut (#17): `slice_ext` is no longer a config key, so the
suite only measures revisions that still have it, such as
`archive/prototype-2026-09-30`; on a later head both variants are the same
build with an unknown key.

Measured (`cb-exp-slice-2`, 4 VMs per cell, 5 blocks, cpuset
0-3, default seccomp): on the 7.2 cell every row is `same` (larson16
+0.0% [-0.5, +0.5], larson64 -0.5% [-1.0, +0.0], xmalloc +0.0%, valkey1
-0.1%, valkey1-free -0.4%; margin 2%); the 6.18 control is `same` on
three rows and inconclusive on the two valkey rows (bimodal, -1.0% and
-1.5%). A direct check on the 7.2 host with the allocator's stats print:
the extension is on, but a 5 s larson16 run sees 5 grants, larson64 8,
xmalloc 29, against about 10^9 operations, so the mechanism almost never
fires on these rows; and under the default seccomp profile the grants are
handed back through `sched_yield` (the `rseq_slice_yield` syscall is not
in the profile). A first run with a `cpus = 4` quota instead of a cpuset
(`cb-exp-slice-1`) also came out `same` on four of five 7.2 rows.

## Rows

`python3 bench/cb rows` lists every row with its class, direction, unit
and command. The families:

| family | rows | what they stand for |
|---|---|---|
| mimalloc-bench (`bench/build.sh`, 06a4c4f) | larson1/10/16/32/64/N, larsonN-sized, xmalloc, xmallocN, alloc-test1/N, cfrac, espresso, mstress, glibc-simple, glibc-thread, cache-scratch1/N, cache-thrash1/N, malloc-large, rptestN, sh6benchN, sh8benchN, barnes, gs, z3, lua, rbstress1/N, mleak10/100 | the allocator benchmarks everyone quotes, with mimalloc-bench's arguments; the N rows use `nproc` (`{nproc2}` for shbench) |
| the Valkey model | valkey1, valkey1-free, valkey1-usable, valkey1-write, valkey1-write-zmalloc, valkey1-write-pair | Valkey's allocator-visible shape at a 500 MiB heap (below) |
| churn | churn1024 | compaction: one class at 44% occupancy under churn, scored by its end occupancy (challoc and jemalloc only: mallctl) |
| applications | rocksdb, nginx-proxy, compile-sqlite, lua, ort-mlp | storage engine, proxy at a fixed rate, compiler, ONNX Runtime with the arena off |
| startup | startup-true, -cc, -py, -ruby, -node, -java | exec-to-exit per run over N runs, plus the syscalls of one run (`strace -c`) |
| sidecar | sidecar-fluentbit | resident memory after a burst and an idle minute |
| runtimes | python1(-malloc), python-dict-list/-regex/-object-graph(-malloc), ruby-binary-trees, ruby-hash-aref-str, ruby-array-sample, ruby-sort-block, ruby-gc-churn, node-json-churn, node-server, pg-select100, pg-tpcb100, pg-select500, pg-connect20, jvm-server | the interpreters and servers challoc ships under |
| soaks | valkey-soak, ruby-soak, jvm-soak | 10..20-minute consumer soaks, one variant per VM |
| Fleetbench (`Dockerfile.cb` stage) | fleetbench-proto | the protobuf arena benchmark, geometric mean of real time (the TCMalloc empirical driver needs TCMalloc's MallocExtension) |
| floor | floor-test | `make test` (the cgroup steps in child cgroups, privileged containers only) and `make fuzz` (pass/fail, smoke suites only) |
| thread churn (#94) | larson1-r10, larson1-r1000, larsonN-r10, larsonN-r1000 | larson with 10 and 1000 rounds per thread: steady state vs the cost of one thread switch |
| probe | spe-probe | the perf event sources and, with SPE, a recording of larson1: samples per event, tlb-miss symbols and pages |

Every timed run records the run's peak RSS (`maxrss_mib`); a row whose
`metrics` names it gets an RSS verdict of its own (`mleak100.maxrss_mib`).
Rows of programs that link `libjemalloc.so.2` (Wolfi's ruby and
fluent-bit) build challoc with the `jemalloc-soname` feature, whose soname
satisfies that `DT_NEEDED`, and run only the variants that can replace it
(`allocs`: challoc and the jemalloc builds). Deviations from upstream
arguments are noted in `rows.toml` (rptestN runs 200 loops, the cache N
rows 20000 iterations, both because the upstream values finish too
quickly or too slowly to be a stable row).

**The Valkey model** (`bench/probes/valkey_model.c`): a single-thread
model of the Valkey soak's server. 850k keys (`{valkey_n}`), each a
40-byte entry, a 24-byte key, a 16-byte object and a value drawn uniformly
from 16..1024 bytes plus a 5-byte sds header (about 500 MiB live,
quantum8-like sizes); an initial fill, then `{valkey_secs}/2` seconds of
SET over random keys (allocate the new value and object, free the old
pair; the heap stays full) and as long of DEL:SET 4:1 (the key count
drains toward 20%). `valkey1` frees through `sdallocx` where the
allocator exports it (glibc, mimalloc and tcmalloc fall back to `free`),
`valkey1-free` forces `free`, `valkey1-usable` precedes every free with
`malloc_usable_size` (Valkey's unsized `zfree`). The `-write` rows are the
write phase alone, so a per-operation counter is per SET; `-zmalloc` adds
a usable-size query after every allocation as well, and `-pair` replaces
both with challoc's `challoc_malloc_usable` / `challoc_free_usable`. Scored
by the write phase's M ops/s; the delete phase and the RSS are in the
sample's `extra`. By hand: `LD_PRELOAD=target/release/libchalloc.so
target/cb/tools/valkey_model -n 100000 -t 3 -s`.

Adding a row: a `[row.<name>]` table in `rows.toml` (a program and a
parser, or a driver and its metric), a program in `bench/build.sh` if it
needs one (pinned by sha256 or commit), `make bench-image` when the image
changes, then `cb launch smoke --rows <name> --variants head,jemalloc54
--controls glibc --cells ref-arm64,ref-amd64` until every run scores.

## Attribution

A fast-path change names the mechanism it attacks and shows it on the
workload it is meant to help (CONTRIBUTING.md, "Performance rules"):

- **Counters per run.** On cells with a PMU (`ref-*` request it; the
  `perfmon` container profile grants `CAP_PERFMON`) every sample carries
  `perf_event_open` cycles, instructions, branch and cache misses
  (`bench/tools/rt.c`), and the report prints instructions and cycles per
  operation for rows with an operation count, with `placement?` where the
  time moved and the instructions did not. `cb report <dir>` re-renders a
  run; `samples.jsonl` has the raw counts.
- **Where the cycles go.** `bench/sampler.c` (built into the image as
  `sampler.so`) is a SIGPROF PC sampler preloaded after the allocator; it
  writes `count module offset` lines, enough to split a row's time
  between the allocator and the program without `perf` in the container.
  For `perf record`, run the row's command in the bench image on a VM
  host with `linux-tools` and attach `sudo perf` from the host.
- **Sidecars** in `bench/probes/` for a soak on a VM: `valkey-perf-sidecar.sh`
  (root on the host: `perf stat -I 1000` per server, `perf record`
  windows), `valkey-defrag-sidecar.sh`, `statsdump.c` (an `atexit`
  `malloc_stats_print`) and `arenasample.c` (per-second arena RSS).

## Operating on GCE

- **Where.** The project, bucket and VM service account come from the
  internal overlay `bench/cells/internal-project.toml` (or `CB_PROJECT`,
  `CB_BUCKET`, `CB_SERVICE_ACCOUNT`, or the flags). Cells list their zones;
  the reference shapes are `c4a-standard-16` (Axion) and `c4-standard-16`
  (Xeon 8581C) in `us-east4-a`. A spot C4/C4A VM costs about $0.30 an
  hour; `cb launch` prints its estimate and refuses a launch over the
  suite's `budget_usd` without `--allow-cost`.
- **Caps.** At most `--max-vms` / `CB_MAX_VMS` (32) VMs of one launch at
  once and `CB_PROJECT_MAX_VCPUS` (2000) vCPUs of cb VMs in the project,
  every session's launches counted; the rest wait. Raise the project cap
  only with the project's quota in mind.
- **Cleanup.** Every VM is labelled `purpose=challoc-bench,cb-run=<run>`
  and created with a `max-run-duration` that deletes it; the launcher
  deletes each VM as soon as its marker lands and everything left when it
  exits. `cb gc --run <run>` deletes one run's VMs after a killed
  launcher; plain `cb gc` only deletes VMs no live launch can want.
- **The image.** `make bench-image` (bench/cb/image.sh) builds and pushes
  per architecture and rewrites `bench/internal-image.lock`; it needs
  Docker and a registry login, so on a Mac run it on a Linux dev host
  (`scripts/devvm.sh`) with a short-lived token. A VM whose pull fails
  builds the image itself (about 10 more minutes; `--no-image` forces
  it).
- **Cell families** (`python3 bench/cb cells`): `gate`/`ref` (the
  reference pair), `cpu-ladder` (one shape per CPU family on the same
  Ubuntu image: Axion, N4A, Turin, Sapphire Rapids, Genoa, Altra),
  `exp-floor` and `tripwire` (public kernel images from 5.10 to 7.x,
  default and privileged containers, 64 KiB pages), `exp-narenas`
  (CPU-quota containers) and whatever cells an internal overlay
  (`bench/cells/internal-*.toml`) adds. `cb cells --resolve` checks that
  every family still resolves; a family that does not makes its cell
  PENDING, never a failure.
- **Traps.** GCE NICs have a 1460-byte MTU (boot.sh sets Docker's);
  metal shapes have no spot and boot for about 11 minutes; C3, C3D, N2D
  and C2 refuse the PMU flag, so their samples have no counters.

## Files

- `rows.toml`, `suites/*.toml`, `cells/*.toml`: what to run, how, where.
- `cb/`: the runner (`run.py` the executor, `gce.py` the launcher,
  `boot.sh` a VM's startup script, `stats.py` and `report.py` the
  verdicts, `image.sh` the image, `perf-build.sh` and `perf-upload.sh` the
  perf bundle, `tests/` its unit tests).
- `build.sh`: every benchmark program, pinned (its header lists them);
  `Dockerfile.cb`: the bench image (Dockerfile.dev's `test` stage, the
  stock allocators, `build.sh`'s output, Fleetbench from its own stage,
  the runtimes); `internal-image.lock`: the image per architecture by
  digest, and the perf bundle per architecture by object and sha256.
- `drivers/`: the driver rows; `cb.sh` is what the short drivers share,
  `lib.sh` what the soak drivers share (they also run by hand: `make run
  CMD='sh bench/drivers/valkey-soak.sh'` with the variables in each
  script's header).
- `tools/`: `rt.c` (the run timer and counters; `rt --probe` checks the
  PMU), `sentinel.c` (the block health probe), `execloop.c` (the startup
  loop).
- `probes/`: the programs rows run (`valkey_model.c`, `churn.c`,
  `ort/mlp.py`, `python/*.py`, `startup/hello200.c`) and the sidecars
  above; `ruby/gc_churn.rb`, `node/*.js`: the runtime rows' scripts.
- `sampler.c`: the PC sampler (above).
- A run's own output is `target/results/cb/<run>/`; numbers of record
  go in the pull request or the issue, with the run id.

## Measurement hygiene

- **`memory.peak` per phase.** From 6.12 a write to a cgroup's
  `memory.peak` resets it for the writing file description, so
  `bench/probes/cgpeak.c` holds one descriptor per soak and the Valkey
  driver reports the peak per phase next to VmHWM. It needs the container
  in the host's cgroup namespace with `/sys/fs/cgroup` writable
  (`CGROUP_PEAK=1` on a `make run`); under `cb launch` the column reads
  `-` and VmHWM is the figure.
- **Guard slabs through `PAGEMAP_SCAN`.** The `api::` tests
  `c_pagemap_guard_{on,off}` (in `make test`) find the guard slabs of
  four classes without touching them (6.16+: `PAGE_IS_GUARD` on the guard
  page only); the floor suite runs them on every kernel image.
- **A quiet host.** `cb run` waits for the load to settle, a sentinel
  opens every block and blocks with steal above 1% are excluded; on a
  shared machine, check `docker ps` for another session's containers
  before trusting a local number.
