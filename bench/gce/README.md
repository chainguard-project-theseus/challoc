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
bench/gce/challoc-bench quick-perf arm64 [OLD=HEAD~1]   # per-operation counters for larson1 and
                                                    # valkey1 under old, new, jemalloc (perf on the host)
bench/gce/challoc-bench scorecard arm64             # the trend line for this commit, fetched back
```

`quick-perf` is the `PERF=1` half of `quick.sh` for a machine where perf
lives on the host: it syncs, runs `make quick QUICK_BUILD_ONLY=1` (the old
and new libraries into `target/quick/`), then `bench/gce/quick-perf.sh` on
the VM host starts each (row, variant) detached in the container with
`sleep 1; exec env LD_PRELOAD=... <row>`, attaches `sudo perf stat -x,
--inherit -p <container init pid>` before the exec, waits for the
container, stops perf by killing its sleep child and prints cycles,
instructions, L1D and dTLB misses per operation (raw files in
`~/logs/quick-perf`). Extra `ROWS=`, `VARIANTS=`, `EVENTS=` arguments go to
that script; `OLD=`/`NEW=` to the build. `scorecard` runs `make scorecard`
(`bench/scorecard.sh`: the fixed row set under challoc and jemalloc, 3
rounds, one line appended to `bench/results/scorecard-<arch>.md`) and then
`fetch`, so the appended line lands in the local file without clobbering a
newer local edit. Run it on both VMs after landing a change and commit the
two lines with the PROGRESS commit.

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

## The kernel matrix (Phase 2.5 task 3)

`challoc-bench matrix` proves the floor on real kernels: for each cell it
creates a throwaway VM `challoc-matrix-<cell>` (labels
`purpose=challoc-matrix,arch=...,cell=...`), provisions Docker (the
`provision` steps minus perf), syncs the tree, runs `bench/gce/matrix-cell.sh`
detached on the VM (`make image`, `make test`, the probe line of
`tests/c/features.c` under `LD_PRELOAD` in the container, `make fuzz`,
`make test-cgroup`, and on the cell marked `privileged` a second `make test`
with `DOCKER_FLAGS=--privileged`, so Docker's seccomp profile does not hide
`process_madvise` and `mseal`), fetches `~/logs` into
`bench/results/matrix/<date>-<cell>.log` and deletes the VM, passed, failed,
timed out or interrupted (the cell's own exit trap, plus a sweep of every
`challoc-matrix-*` VM in the zone when the driver exits; `matrix clean` runs
the sweep by hand). The machines are small and measure nothing.

```
bench/gce/challoc-bench matrix list        # the cells
bench/gce/challoc-bench matrix             # every cell, MATRIX_JOBS (3) at a time,
                                           # MATRIX_TIMEOUT (2700 s) each
bench/gce/challoc-bench matrix k510-amd64 64k-arm64
bench/gce/challoc-bench matrix clean       # delete every challoc-matrix-* VM
bench/gce/challoc-bench matrix lab 64k-arm64   # create, prep, provision, sync; keep the VM
bench/gce/challoc-bench matrix ssh 64k-arm64 'cd ~/challoc && make test-cgroup-oscillate OSC_SECS=60'
bench/gce/challoc-bench matrix sync 64k-arm64  # rsync the tree again
```

`matrix lab` runs a cell up to the sync and keeps the VM for experiments
(its log goes to `.tmp-matrix-lab/`, not `bench/results/matrix/`); `matrix
ssh` and `matrix sync` drive it, `matrix clean` (or running the cell for
real, which recreates the VM under the same name) deletes it. A lab VM
measures nothing either; it exists to read `memory.stat`, the allocator's
report and the like on a kernel the reference VMs do not have.

| cell       | image family            | kernel                         | machine        | notes |
|------------|-------------------------|--------------------------------|----------------|-------|
| k510-amd64 | `debian-11` (deprecated; the newest READY image is used) | 5.10 `cloud-amd64` | e2-standard-4 | bullseye's LTS ended 2026-08-31: provision drops the security suite and installs from main |
| k61-amd64  | `debian-12`             | 6.1 `cloud-amd64`              | e2-standard-4  | |
| k61-arm64  | `debian-12-arm64`       | 6.1 `cloud-arm64`              | c4a-standard-4 | |
| k68-amd64  | `ubuntu-2404-lts-amd64` | `linux-generic` (6.8 GA)       | c3-standard-4  | prep: install, point GRUB at it by title, reboot; plus the `--privileged` `make test` |
| k68-arm64  | `ubuntu-2404-lts-arm64` | `linux-generic` (6.8 GA)       | c4a-standard-4 | prep as above |
| hwe-amd64  | `ubuntu-2404-lts-amd64` | `linux-generic-hwe-24.04` (6.13+, not a GCE kernel; 7.0.0-31 in 2026-09) | c3-standard-4 | prep as above; plus the `--privileged` `make test` (the batch purge path and mseal for real) |
| 64k-arm64  | `ubuntu-2404-lts-arm64` | `linux-generic-64k` (6.8 GA, 64 KiB pages) | c4a-standard-4 | prep as above; `getconf PAGESIZE` must print 65536 |
| gcp-amd64  | `ubuntu-2404-lts-amd64` | the image's `linux-gcp` (7.0.0-1011-gcp, the reference VMs' kernel) | c3-standard-4 | |
| gcp-arm64  | `ubuntu-2404-lts-arm64` | the same                       | c4a-standard-4 | |

The Ubuntu 24.04 GCE images boot the 7.0 `linux-gcp` kernel since 2026, so
the 6.8 cells install the GA `linux-generic` kernel on top. `prep` cells set
`GRUB_DEFAULT` in `/etc/default/grub.d/99-challoc-matrix.cfg` by menu title
because the GCE kernel's version string (`7.0.0-10xx-gcp`) sorts above the
generic one and would stay the default, and clear `GRUB_FORCE_PARTUUID`
there: the GCE image boots without an initrd, which only the gcp kernel
(storage drivers built in) survives; a generic kernel panics with "Unable
to mount root fs" until the initrd is back. Debian's `core_pattern` is
`core` in the cwd, so provisioning sets `kernel.core_pattern=|/bin/false`
(an aborting negative test would otherwise leave a root-owned `core` in
the bind-mounted tree and break the next `docker build`); it sets the one
key rather than `sysctl --system`, which would re-apply GCE's
`net.ipv4.ip_forward = 0` over Docker's and cut the containers off. Matrix VMs keep no host keys
(`UserKnownHostsFile=/dev/null`): GCE recycles a deleted VM's external IP
within minutes, and `accept-new` against a file then rejects the new key. Two
drivers must not run at once with the same prefix: the exit sweep deletes
every VM of the prefix; `CHALLOC_MATRIX_PREFIX` names another.

The last lines of each VM log are machine-readable: `CPU <model>`,
`PROBE features: ...`, `PROBE_PRIVILEGED features: ...`, `SCORECARD | ... |`
(CPU cells) and `MATRIX cell=... kernel=... page=... machine=... zone=...
vcpus=... tests=<passed>/<failed> fuzz=... cgroup=... privileged=...
scorecard=... wall=... outcome=PASS|FAIL`; the driver prints them as the
summary. Results: `bench/results/<date>-matrix.md`.

## The CPU matrix (Phase 2.5 task 4)

`challoc-bench matrix cpu` runs the `cpu-*` cells: the same Ubuntu 24.04
image and its GCE kernel on one machine type per CPU family, so only the
CPU varies. A CPU cell runs `make image`, `make test`, the two probe lines,
the CPU capture (`cpuinfo.log`: model name, the `flags`/`Features` line,
`lscpu`, and the raw `cpuid`/HWCAP registers of `bench/gce/cpuid-dump.c`,
the source of the host fixtures in `src/sys/cpu.rs`), and, instead of fuzz
and test-cgroup, one `make scorecard ROUNDS=3` (challoc versus jemalloc, the
fixed row set) written to `bench/results/scorecard-<family>.md` on the VM
and fetched back next to the reference files. `scorecard-aarch64.md` and
`scorecard-x86_64.md` stay the acceptance record (C4A and C4 at 16 vCPU)
and are not touched; the per-CPU files are 8 vCPU, so compare them with
the reference CPU of the same architecture row by row (challoc-vs-jemalloc
percentages), never by absolute throughput.

Cells name their zones (a comma-separated list, tried in order, at most
two): every family is in `us-east4-a` except T2A (`us-central1-a,b,f`).
A zone that is out of capacity or quota for the family
(`ZONE_RESOURCE_POOL_EXHAUSTED`, `QUOTA_EXCEEDED`) is recorded in the
cell's log and the next zone tried; with no zone left the cell's line reads
`outcome=PENDING reason=create:<error>`. `matrix clean` and the exit sweep
delete matrix VMs in every zone. C4, C4A and N4 take Hyperdisk Balanced
only; C2, N2D and T2A take Persistent Disk only (the driver picks per
family).

| cell    | machine        | CPU                                  | zones                       |
|---------|----------------|--------------------------------------|-----------------------------|
| cpu-c4a | c4a-standard-8 | Google Axion, Neoverse V2 (reference architecture, for the record) | us-east4-a, us-east4-b |
| cpu-t2a | t2a-standard-8 | Ampere Altra, Neoverse N1            | us-central1-a, us-central1-b |
| cpu-c4  | c4-standard-8  | Intel Xeon 8581C, Emerald Rapids (reference) | us-east4-a, us-east4-b |
| cpu-c3  | c3-standard-8  | Intel Sapphire Rapids                | us-east4-a, us-east4-b      |
| cpu-c3d | c3d-standard-8 | AMD Genoa, Zen 4                     | us-east4-a, us-east4-b      |
| cpu-n2d | n2d-standard-8 | AMD Milan or Rome, whichever the zone gives (the log records the model) | us-east4-a, us-east4-b |
| cpu-c2  | c2-standard-8  | Intel Cascade Lake                   | us-east4-a, us-east4-b      |
| cpu-n4  | n4-standard-8  | Intel Emerald Rapids on the N4 platform (optional) | us-east4-a, us-east4-b |

```
bench/gce/challoc-bench matrix cpu                 # every cpu-* cell, 3 at a time
MATRIX_TIMEOUT=3600 bench/gce/challoc-bench matrix cpu-c3d cpu-t2a
```

Results: `bench/results/<date>-cpu-matrix.md` (one line per cell, the
feature table across CPUs, the scorecard table across CPUs) and
`bench/results/scorecard-<family>.md`.

## Recipes for the traps

- A `challoc-bench ssh <vm> --command='... setsid nohup make ... &'` that
  launches a detached run does not return until the run ends (gcloud's ssh
  keeps the session open for the job's process group even with every fd
  redirected), and a loop over VMs then starts the next VM's leg late, or
  twice (the bench lock refuses the duplicate). Launch detached jobs over
  direct ssh with the job in a subshell: `ssh -T -i
  ~/.ssh/google_compute_engine <user>@<ip> 'cd challoc && (setsid nohup make
  ab ... > ~/logs/ab.log 2>&1 < /dev/null &); sleep 2; tail -1 ~/logs/ab.log'`
  returns at once. When polling with `pgrep -f`, exclude the polling shell
  itself (`pgrep -f "[m]ake ab"`), whose command line contains the pattern.
- gcloud cannot refresh its auth token (`Reauthentication failed` after the
  token expires, or `NameResolutionError` for `oauth2.googleapis.com` from
  gcloud's Python while the shell resolves it fine): `status`, `pmu-check`,
  `run`, `quick`, `ab` and `scorecard` go through `gcloud compute ssh` and
  fail, while `sync` and `fetch` (direct `rsync`/`ssh` with the minted key)
  keep working. Fall back to direct ssh for the rest, `ssh -T -i
  ~/.ssh/google_compute_engine <user>@<ip>` with the IP from the last
  `status`, run the same `make` targets from `~/challoc` there (the
  `pmu-check` body is the `perf stat -x,` loop in `cmd_pmu_check`), and note
  the fallback in the results file. Never stop the session's VMs from a
  fallback shell.
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
