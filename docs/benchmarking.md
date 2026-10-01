# Benchmarking challoc

This is how challoc is measured: where reference numbers come from, the
protocol every recorded number follows, and the rules for reading a
result. The harness is `bench/cb`; its commands, rows, suites, cells and
knobs are in `bench/README.md`.

## Where numbers come from

- **Reference cells.** Reference numbers and hardware counters come from
  the two gate cells of `bench/cells/public.toml`: C4A (Google Axion,
  aarch64) and C4 (Intel Xeon 8581C, x86_64), both `*-standard-16`,
  created with `--performance-monitoring-unit=standard` so every run
  carries cycles, instructions, branch and cache misses. `cb launch`
  creates fresh spot VMs per run (several per cell, one VM is one host in
  the statistics), runs every benchmark inside the pinned bench image
  (`bench/Dockerfile.cb`), so the toolchain and the stock allocators are
  the same everywhere, and deletes the VMs when their results are in.
- **The Mac is for development.** The dev container on Apple silicon is
  for correctness and for same-session A/B rounds only. A Mac number and a
  GCE number are never compared with each other.
- **Emulated x86_64 is never used for numbers.** `PLATFORM=linux/amd64`
  on Apple silicon runs under emulation and is for correctness only.
- **The supported floor and the ceiling.** The floor is a Chainguard OS
  (Wolfi, glibc 2.44+) container on any host kernel from 5.14 up; the
  kernel and CPU matrices prove it on real kernels, including 64 KiB-page
  aarch64. The ceiling is the same binary lighting every kernel and CPU
  feature the host offers, probed at runtime: the compiler baseline stays
  where Wolfi sets it (never `-C target-cpu`). A feature that is on by
  default when present must be within noise on the reference VMs when
  absent and must not change observable behaviour (allocation results,
  abort conditions) between hosts, only cost, memory and detection. Every
  probed feature gets a test on a host that has it, a test on a host that
  lacks it, a cost row and a line in the feature report.

## The protocol

- Container: the bench image, pinned by digest per architecture
  (`bench/internal-image.lock`); every sample's comparability key names
  the image, the kernel, the CPU, the machine type and the container
  profile, and runs with different keys are never pooled.
- The same binary of each benchmark program runs under every allocator;
  only `LD_PRELOAD` (or the consumer's link) changes. The stock allocators
  are glibc 2.44's malloc, Wolfi's jemalloc 5.3.1, jemalloc 5.3.0 and
  5.4.0 from the release tarballs, mimalloc 3.x and Wolfi's
  tcmalloc-minimal.
- Paired blocks: in every block each row runs every variant twice at
  mirror positions (ABBA, a Williams sequence for more variants), one
  fresh process per run with a random environment pad, after discarded
  warm-up blocks; a sentinel opens every block and blocks with steal
  above 1% are excluded.
- The verdict per row is the mean paired log contrast with a Student-t
  interval over the hosts (VMs), against the row class's margin (1%
  single-thread, 2% multithread, 3% server and soak, 2% RSS): `better`,
  `worse`, `same`, `sub-threshold`, `inconclusive` or `broken`, and a
  headline over the gate rows with Holm's correction.
- Every variant a comparison needs is measured in the same run, on the
  same VMs. Numbers from different runs are compared only as same-run
  ratios.

## The tiers

- **Go/no-go** (`make quick`, `cb launch quick`): head against base on
  both gate cells, 3 VMs per cell, a second look when the first is
  inconclusive; about 6 minutes from cold. Its verdict is what a pull
  request cites.
- **Suites of record** (`cb launch full`, `apps`, `soak`, an experiment
  suite): the general-purpose baseline against jemalloc 5.4.0 and glibc,
  the application rows, the 20-minute consumer soaks. A results file in
  `bench/results/` is written from one of these runs and cites its run id.
- **Calibration** (`cb launch aa`): base against itself; it measures the
  false-positive rate (`cb fp`) and calibrates the row margins.

A local `cb run` (the dev container) is indicative only: its
comparability key names the host, and it is for the plumbing and a first
look, never for a decision.

## Attribution before fast-path changes

A change to the fast paths (`src/heap/`, `src/class/`, `src/slab.rs`,
`src/api/*.rs`) that is meant to help a target workload is preceded by a
per-operation cycle attribution on that workload (the per-run counters
cb records on the gate cells, instructions and cycles per operation in
the report, the soak's perf sidecar `bench/probes/valkey-perf-sidecar.sh`,
or `bench/sampler.so`),
recorded in the results file, and the change names the mechanism it
attacks. larson counters do not stand in for Valkey or Ruby: their working
sets are a few MiB, Valkey's is about 550 MiB.

A cleanup that is not meant to change the fast paths proves it with
`scripts/codegen-diff.sh` (zero changed functions on the hot set) rather
than with a benchmark.

## Consumer soaks

- **Valkey** (row `valkey-soak`, suite `soak`, `bench/drivers/valkey-soak.sh`): write-heavy then
  delete-heavy phases with `valkey-benchmark` and `memtier_benchmark` for 20
  minutes against challoc and Valkey's bundled jemalloc, `INFO memory`
  sampled every 10 s. The criterion: throughput and p99 at least equal to
  bundled jemalloc, peak RSS no more than 5% above it, and post-delete RSS
  back within 10% of jemalloc's after the decay window. The soak's
  throughput columns carry 3 to 5% of generator noise between runs; the
  tie-breaker for "at least equal" is cycles per command from the perf
  sidecar, together with the `valkey1` row.
- **Stock configuration.** The soak criteria are met by the allocator
  under Valkey's stock configuration. A `USE_CHALLOC` value in the Valkey
  patch is allowed only where a Valkey constant encodes jemalloc's slab
  geometry (the defrag tests' thresholds), and it names the constant and
  the measurement behind it; it is never how a soak criterion is met.
- **Ruby** (the `ruby-*` and `rbstress` rows, the `ruby-soak` soak),
  **Python** (`python*`), the **JVM** (`jvm-server`, `jvm-soak`),
  **Node.js** (`node-*`) and **PostgreSQL** (`pg-*`): see
  `bench/README.md` for rows and knobs. Within 10% of the best stock allocator on time and RSS is the
  bar; a row outside it is attributed (the mechanism named) or recorded as
  an open issue.

## Results files

A results file in `bench/results/` cites the cb run id and the command
that produced it (`target/results/cb/<run>/report.md` has the hosts, the
image, the variants and every row's estimate and interval), the commit or
release tag measured, and, for an attributed gap, the counters or samples
behind the attribution.
