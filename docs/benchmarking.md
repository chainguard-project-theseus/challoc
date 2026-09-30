# Benchmarking challoc

This is how challoc is measured: where reference numbers come from, the
protocol every recorded number follows, and the rules for reading a
result. The commands, rows and knobs are in `bench/README.md`; the GCE
driver and the kernel, CPU and Chainguard OS matrices are in
`bench/gce/README.md`.

## Where numbers come from

- **Reference machines.** Reference numbers and hardware counters come
  from two dedicated Google Compute Engine VMs, one per architecture:
  C4A (Google Axion, aarch64) and C4 (Intel Xeon, x86_64), both
  `*-standard-16`, created with `--performance-monitoring-unit=standard`
  so `perf` sees cycles, instructions, branch misses, L1D and dTLB/iTLB
  misses. `bench/gce/challoc-bench` creates, provisions, syncs, runs and
  stops them; every benchmark runs inside the repository's
  `Dockerfile.dev` image on the VM, so the toolchain and the stock
  allocators are the same everywhere. Stop every VM you start.
- **The Mac is for development.** The dev container on Apple silicon is
  for correctness and for same-session A/B rounds only. A Mac number and a
  GCE number are never compared with each other.
- **Emulated x86_64 is never used for numbers.** `PLATFORM=linux/amd64`
  on Apple silicon runs under emulation and is for correctness only.
- **The supported floor and the ceiling.** The floor is a Chainguard OS
  (Wolfi, glibc 2.44+) container on any host kernel from 5.10 up; the
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

- Container: Wolfi base with pinned toolchain versions; the results file
  records the toolchain, the kernel version, the CPU model and the machine
  type.
- The same binary of each benchmark program runs under every allocator;
  only `LD_PRELOAD` (or the consumer's link) changes. The stock allocators
  are glibc 2.44's malloc, Wolfi's jemalloc 5.3, mimalloc 3.x and Wolfi's
  tcmalloc-minimal.
- A warm-up run is discarded. Time is the median of 5 runs; memory is the
  max RSS from `bench/rt` (the `/usr/bin/time -v` figure). Variants are
  interleaved round by round, and results files show the sorted list of
  every row's scores, not only the median, so overlap is visible.
- Every variant a comparison needs is measured in the same run. Numbers
  from different runs, days or hosts are compared only as same-run ratios.
- Check that the host is quiet before trusting a number: load average,
  `docker ps` (a `ryuk` container is someone else's test run), no second
  benchmark on the same VM.

## Two tiers

- **Fast tier** (`make quick`, `challoc-bench quick <vm>`): about two and
  a half minutes; one round of larson 1/10, xmalloc-test, alloc-test 1,
  the Valkey model `valkey1` (`bench/probes/valkey_model.c`) and the churn
  probe, under the old build, the new build and jemalloc, with a go/no-go
  exit status. It answers "is this variant worth the protocol run?" and
  nothing else: its numbers are never recorded in `bench/results/` or held
  against a criterion. `PERF=1` (or `challoc-bench quick-perf <vm>` on the
  VMs) adds cycles, instructions, L1D and dTLB misses per operation.
- **Protocol tier** (`make ab`, `challoc-bench ab <vm>`): 5 rounds plus
  warm-up, interleaved variants, sorted scores, median RSS. This is the
  only source of numbers for `bench/results/` and for acceptance.
  Cached stock-allocator baselines (`bench/baseline.sh`,
  `BASELINES=cached`) save time in iteration runs and are revalidated by a
  jemalloc sanity round each time; acceptance runs do not use them.

Always the fast tier first; the protocol tier only for a variant that
passed it. During a working session the VMs can stay warm
(`challoc-bench session start|stop`, `CHALLOC_GCE_WARM=1`).

## The scorecard

`bench/scorecard.sh` (`make scorecard`, `challoc-bench scorecard <vm>`)
runs a fixed row set (larson 1/10/16, xmalloc-test, alloc-test 1/N,
valkey1, valkey1-free, cfrac, espresso, mstress, glibc-simple, redis)
under challoc and jemalloc, 3 interleaved rounds, and appends one line per
(date, commit, arch) to `bench/results/scorecard-<arch>.md`: percentages
against jemalloc, the RSS ratios and the commit's most recent soak.
`bench/scorecard.sh --render` prints the trend. The scorecard is a trend,
not an acceptance measurement; criteria are judged from same-run protocol
files. A 3-round row carries a few percent of VM noise; read a single
line's move against the column's history before treating it as real.

## Attribution before fast-path changes

A change to the fast paths (`src/heap/`, `src/class/`, `src/slab.rs`,
`src/api/*.rs`) that is meant to help a target workload is preceded by a
per-operation cycle attribution on that workload (`perf stat` /
`perf annotate` on the VM, `challoc-bench quick-perf`, the soak's perf
sidecar `bench/probes/valkey-perf-sidecar.sh`, or `bench/sampler.so`),
recorded in the results file, and the change names the mechanism it
attacks. larson counters do not stand in for Valkey or Ruby: their working
sets are a few MiB, Valkey's is about 550 MiB.

A cleanup that is not meant to change the fast paths proves it with
`scripts/codegen-diff.sh` (zero changed functions on the hot set) rather
than with a benchmark.

## Consumer soaks

- **Valkey** (`make valkey-soak`, `bench/valkey-soak.sh`): write-heavy then
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
- **Ruby** (`make ruby-bench`, `make ruby-soak`), **Python**
  (`make python-bench`), the **JVM** (`make jvm-bench`, `make jvm-soak`),
  **Node.js** and **PostgreSQL**: see `bench/README.md` for rows and
  knobs. Within 10% of the best stock allocator on time and RSS is the
  bar; a row outside it is attributed (the mechanism named) or recorded as
  an open issue.

## Results files

A results file in `bench/results/` records the host (machine type, CPU
model, kernel), the toolchain, the commit or release tag measured, the
command that produced it, every variant's sorted scores and median RSS,
and, for an attributed gap, the counters or samples behind the
attribution.
