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
bench/gce/challoc-bench quick arm64 OLD=HEAD~1   # sync, then `make quick` (fast tier)
bench/gce/challoc-bench ab arm64 VARIANTS='old=HEAD~1 new=HEAD jemalloc' ROUNDS=5
                                           # sync, then `make ab` (protocol tier)
bench/gce/challoc-bench fetch arm64        # pull bench/results/* back (never overwrites newer local files)
bench/gce/challoc-bench ssh arm64
bench/gce/challoc-bench status
bench/gce/challoc-bench stop all           # do this before an iteration ends (unless CHALLOC_GCE_WARM, below)
```

The VMs run the repository's own `Dockerfile.dev` image through the same
`Makefile` as the Mac, so the toolchain is pinned identically. A full
`make test` (image build, release build, both test groups) takes about 100 s
on either VM.

## Two tiers on a VM

`bench/README.md` describes the tiers; on a VM they are driven as

```
bench/gce/challoc-bench quick arm64                 # working tree vs HEAD, ~2 min, exit 1 = no-go
bench/gce/challoc-bench quick arm64 OLD=HEAD~1      # a committed change vs its parent
bench/gce/challoc-bench ab arm64 VARIANTS='old=HEAD~1 new=HEAD narenas16=HEAD~1:narenas:16 jemalloc' \
    RESULT=bench/results/2026-09-17-aarch64-gce-<topic>.md
bench/gce/challoc-bench fetch arm64                 # bring the results file back
bench/gce/challoc-bench run arm64 'make baseline'   # once per VM: cached stock-allocator rows
bench/gce/challoc-bench quick arm64 BASELINES=cached
```

Every extra argument is a `VAR=value` for `make` and is quoted for the
remote shell, so spaces inside `VARIANTS` survive. Both subcommands `sync`
first; `sync` mirrors the working tree including uncommitted changes and
prints a warning listing any uncommitted change under `src/` (or the crate
manifest), because the library the VM then measures as `tree` is not HEAD.
Run the iteration's `quick` before any `ab`; a variant that fails `quick` is
not taken to the protocol tier (`scripts/ralph-prompt.md` step 4).

Long runs (`ab` with 5 rounds is 6 to 8 minutes, `make bench` 13, the soak
45) survive a dropped ssh session when launched detached on the VM:
`challoc-bench ssh arm64 --command='cd challoc && setsid nohup make ab ... >
~/logs/ab.log 2>&1 < /dev/null &' -- -T`, then poll `~/logs/ab.log`. Keep
VM-side logs in `~/logs`, not under `~/challoc`: `sync` is `rsync --delete`.

## Sessions and warm VMs

Starting a VM takes a minute or two, and a stopped VM comes back with a new
external IP. When a driving session runs many iterations back to back the
session, not each iteration, owns the VMs:

```
bench/gce/challoc-bench session start all   # once, at the start of the session
export CHALLOC_GCE_WARM=1                    # for every iteration the session launches
...                                          # iterations: quick, ab, fetch; never start/stop
bench/gce/challoc-bench session stop all    # once, at the end; nothing may stay running
```

With `CHALLOC_GCE_WARM` set, `challoc-bench start` and `stop` print a note
and do nothing, so an iteration that follows the "stop what you started"
rule from CLAUDE.md cannot tear down the session's VMs; `session stop`
ignores the variable. Without the variable the old rule holds: every
iteration that starts a VM stops it before it ends, and `status` is checked
at the start of an iteration for orphans.

## Recipes for the traps

- Host key changed after a restart (`Host key verification failed` from
  `sync`, `fetch` or `ssh`): the VM has a new external IP whose key collides
  with a stale entry. `ssh-keygen -R <ip> -f ~/.ssh/google_compute_known_hosts`,
  where `<ip>` is from `challoc-bench status`, then retry.
- `fetch` uses `rsync --update -i`: a local results file that is newer than
  the VM's copy is left alone (and the transfer list shows what moved), so a
  file edited locally after the run is never clobbered. A VM-side file that
  is newer still wins; commit local edits before fetching if in doubt.
- Stale mtimes: files restored with `mv`/`cp -p` or arriving by `rsync -a`
  can be older than cargo's last build and are skipped; `bench/lib.sh`
  touches `src/lib.rs` before building the `tree` variant, and `git archive`
  variants are always fresh. When building by hand, `touch src/lib.rs`.
- `perf` must run as root to attach to the container's process
  (`perf-stat` already does `sudo perf`); `perf_event_paranoid=-1` alone is
  not enough for another user's process.
- The first `make -j16` of the jemalloc baseline Valkey inside
  `valkey-soak.sh` has failed on a fresh clone and succeeded on retry; the
  script now exits when that build fails instead of continuing without the
  jemalloc half.

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
