# GCE performance lab

Two Compute Engine VMs give challoc a quiet, dedicated Linux host per
architecture with real hardware performance counters. They are the reference
environment for benchmark numbers (docs/plan.md, Phase 1.5); the Docker VM on
the Mac stays the development environment, and a Mac number is never compared
with a GCE number.

| target | instance            | machine type      | CPU                              | PMU device      |
|--------|---------------------|-------------------|----------------------------------|-----------------|
| arm64  | challoc-bench-arm64 | c4a-standard-16   | Google Axion, Neoverse V2        | `armv8_pmuv3_0` |
| amd64  | challoc-bench-amd64 | c4-standard-16    | Intel Xeon Platinum 8581C, 2.3 GHz | `cpu`         |

Both: 16 vCPUs, about 60 GB RAM, Ubuntu 24.04 (kernel `7.0.0-1011-gcp`),
4 KiB pages, THP `madvise`, `vm.max_map_count` 1048576, created with
`--performance-monitoring-unit=standard`. Zone `us-east4-a` (Ashburn; moved
from `us-central1-b` on 2026-09-17 after weeks of `ZONE_RESOURCE_POOL_EXHAUSTED`
on starts; both machine types, the same CPU models and kernel, `pmu-check`
green), project from
the active `gcloud` configuration (`CHALLOC_GCE_PROJECT` overrides).

## Driver

```
bench/gce/challoc-bench create all         # both VMs, hyperdisk-balanced boot disks
bench/gce/challoc-bench provision all      # Docker (MTU 1460), make, perf, rsync, sysctls
bench/gce/challoc-bench pmu-check all      # six reference events must count non-zero
bench/gce/challoc-bench sync all           # rsync the working tree to ~/challoc on each VM
bench/gce/challoc-bench run arm64 'make test'
bench/gce/challoc-bench make amd64 bench   # sync, then `make bench` on the VM
bench/gce/challoc-bench fetch arm64        # pull bench/results/* back
bench/gce/challoc-bench ssh arm64
bench/gce/challoc-bench status
bench/gce/challoc-bench stop all           # do this before an iteration ends
```

The VMs run the repository's own `Dockerfile.dev` image through the same
`Makefile` as the Mac, so the toolchain is pinned identically. A full
`make test` (image build, release build, both test groups) takes about 100 s
on either VM.

## perf

Wolfi has no `perf` package, so `perf` runs on the VM host (Ubuntu's
`linux-tools-$(uname -r)`) and attaches to the container's process:

```
bench/gce/challoc-bench perf-stat arm64 'docker run --rm -v $PWD:/src -w /src \
  -v challoc-target-arm64:/src/target challoc-dev:arm64 \
  sh -c "LD_PRELOAD=/src/target/release/libchalloc.so target/bench-bin/larson 5 8 1000 5000 100 4141 10"'
```

`perf-stat` starts the container detached, attaches `perf stat --inherit -p`
to its init process, and stops when the container exits. For `perf record`
with symbols, mount the container's paths for `perf report --symfs`: the
repository is at `~/challoc` on the host and the target volume at
`/var/lib/docker/volumes/challoc-target-<arch>/_data`.

Reference events, verified on 2026-09-16 on both VMs (`pmu-check`):
`cycles`, `instructions`, `branch-misses`, `L1-dcache-load-misses`,
`dTLB-load-misses`, `iTLB-load-misses`. `kernel.perf_event_paranoid` is -1
and `kernel.kptr_restrict` is 0 on the VMs.

## Gotchas found while setting up

- GCE NICs use a 1460-byte MTU; Docker's bridge defaults to 1500, so `apk`
  and `cargo` fetches from inside a container stall and `apk` reports every
  package as "no such package". `provision` writes `{"mtu": 1460}` to
  `/etc/docker/daemon.json`.
- Ubuntu's cloud image has no `make`; `provision` installs it.
- Direct `rsync`/`ssh` need gcloud's key, `~/.ssh/google_compute_engine`;
  run `challoc-bench ssh arm64 -- true` once to have gcloud mint it.
- A running 16-vCPU C4A or C4 costs roughly $0.70 to $0.90 per hour. Stop
  the VMs when idle; `start` brings them back with the disk intact.
