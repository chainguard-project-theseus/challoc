# GCE performance lab

Two Compute Engine VMs give challoc a quiet, dedicated Linux host per
architecture with real hardware performance counters. They are the reference
environment for benchmark numbers (docs/benchmarking.md); the Docker VM on
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
bench/gce/challoc-bench make amd64 bench-build   # sync, then `make bench-build` on the VM
bench/gce/challoc-bench quick arm64 OLD=HEAD~1   # sync, then `make quick-legacy` (fast tier)
bench/gce/challoc-bench ab arm64 VARIANTS='old=HEAD~1 new=HEAD jemalloc' ROUNDS=5
                                           # sync, then `make ab` (protocol tier)
bench/gce/challoc-bench fetch arm64        # pull target/results/* back (never overwrites newer local files)
bench/gce/challoc-bench ssh arm64
bench/gce/challoc-bench status
bench/gce/challoc-bench stop all           # do this before an iteration ends (unless CHALLOC_GCE_WARM, below)
```

The VMs run the repository's own `Dockerfile.dev` image through the same
`Makefile` as the Mac, so the toolchain is pinned identically. A full
`make test` (image build, release build, both test groups) takes about 100 s
on either VM. `provision` and `sync` run their targets in parallel and exit
with the worst target's status (since 2026-09-20: before that a VM still
booting, or a host key changed by a restart, returned 0 and the next step
ran on an unprovisioned or stale VM). After the copy, `sync` checks that
the VM's HEAD is the local HEAD and that its commit object is readable,
copies once more if not, and fails if the two still disagree: rsync lists
the files before it copies them, so a commit made during a sync leaves the
VM's HEAD naming an object it never received (the `3c6602b+` scorecard
lines of 2026-09-20 came from that: `git diff HEAD` on the VM failed with
"bad object HEAD" and `bench/lib.sh tree_dirty` read the failure as a dirty
tree; `tree_dirty` now stops a run when git cannot answer). Do not commit
while a sync runs, or sync again afterwards.

### Environment

| variable | default | what |
|---|---|---|
| `CHALLOC_GCE_PROJECT` | the active `gcloud` project | project of every VM |
| `CHALLOC_GCE_ZONE` | `us-east4-a` | zone of the reference pair and of every cell whose zone list is `-` |
| `CHALLOC_GCE_PREFIX` | `challoc-bench` | the VM name prefix (`<prefix>-arm64`, `<prefix>-amd64`); a lane's pair |
| `CHALLOC_MATRIX_PREFIX` | `challoc-matrix`, or `<prefix>-matrix` for a lane | the matrix cells' name prefix (the exit sweep deletes every VM under it) |
| `CHALLOC_GCE_SSH_KEY` | `~/.ssh/google_compute_engine` | the key `sync`, `fetch` and the matrix cells use for direct ssh/rsync (gcloud mints it on the first `challoc-bench ssh <vm> -- true`) |
| `CHALLOC_GCE_USER` | `$USER` | the login user on a host without OS Login (the Chainguard OS server images, below) |
| `CHALLOC_GCE_WARM` | unset | set: `start` and `stop` do nothing, the session owns the VMs |
| `MATRIX_JOBS` | 3 | cells at a time |
| `MATRIX_TIMEOUT` | 2700 s | per cell, create to fetched logs (7200 for a `cgos-*` cell) |
| `MATRIX_MACHINE` | the cell's | replaces the cell's machine type (`MATRIX_MACHINE=c4a-standard-16 matrix lab 64k-arm64` for a soak at 64 KiB pages) |
| `MATRIX_PMU` | unset | set: create the cell VM with `--performance-monitoring-unit=standard` (C3, C3D, C4, C4A and N4 take it; T2A, C2 and N2D refuse it), so a lab cell can run `quick-perf.sh` once `linux-tools-$(uname -r)` is installed on it |
| `MATRIX_DATE` | today (`%F`) | the date in the cells' log names and the summary; set it to rerun a cell into an earlier day's set |
| `MATRIX_OUT` | `target/results/matrix` | where the cell logs go (`matrix lab` uses `.tmp-matrix-lab/`) |
| `MATRIX_ADOPT` | unset | `<vm>`: run a cell on that existing VM (any zone), never delete it |

Login: every VM is created with `enable-oslogin=TRUE` and the OS Login
posix user logs in (with sudo and the docker group), except a Chainguard
OS server image (`kirkland-server-hypervisor-*`, the `cgos-arm64` and
`cgos-amd64` cells): a bootc server host takes no OS Login, the VM is
created without the metadata key and the login is `CHALLOC_GCE_USER`
through the project's `ssh-keys` metadata. The workstation images
(`kirkland-workstation-*`, the `cgos72-amd64` cell) invert this again:
their guest agent creates no user from `ssh-keys` metadata, and OS Login
works. `matrix_oslogin` in `challoc-bench` is the one place that decides
(the create flag, the login user of a cell, `matrix ssh`).

## Lanes: one prefix per worktree

Every subcommand names its VMs `$CHALLOC_GCE_PREFIX-<arch>` (default
`challoc-bench`, the reference pair above) and its matrix cells
`$CHALLOC_GCE_PREFIX-matrix-<cell>` (`challoc-matrix-<cell>` for the
default prefix). A parallel worktree ("lane") exports
`CHALLOC_GCE_PREFIX=challoc-<lane>` and gets a fully independent pair
(`create all`, `provision all` once; `stop`/`delete` before its turn ends)
that `sync`, `run`, `quick`, `quick-perf`, `ab`, `fetch` and `matrix` then
address; nothing it runs touches another prefix's VMs. `sync` from a git
worktree ships the repository's common `.git` with the worktree's `HEAD`
and `index`, so the VM holds a plain checkout of the lane's branch and
`git archive` of a rev works there. `status` lists the prefix's VMs when
the variable is set and every `challoc-*` VM when it is not;
`challoc-bench lanes` groups every VM by prefix with status and uptime,
the whole picture. The scorecard is not a lane command: only runs from
`main` on the reference pair append to `target/results/scorecard-<arch>.md`.

## Two tiers on a VM

`bench/README.md` describes the tiers; on a VM they are driven as

```
bench/gce/challoc-bench quick arm64                 # working tree vs HEAD, ~2 min, exit 1 = no-go
bench/gce/challoc-bench quick arm64 OLD=HEAD~1      # a committed change vs its parent
bench/gce/challoc-bench ab arm64 VARIANTS='old=HEAD~1 new=HEAD narenas16=HEAD~1:narenas:16 jemalloc' \
    RESULT=target/results/<date>-aarch64-gce-<topic>.md
bench/gce/challoc-bench fetch arm64                 # bring the results file back
bench/gce/challoc-bench run arm64 'make baseline'   # once per VM: cached stock-allocator rows
bench/gce/challoc-bench quick arm64 BASELINES=cached
bench/gce/challoc-bench quick-perf arm64 [OLD=HEAD~1]   # per-operation counters for larson1 and
                                                    # valkey1 under old, new, jemalloc (perf on the host)
bench/gce/challoc-bench scorecard arm64             # the trend line for this commit, fetched back
```

`quick-perf` is the `PERF=1` half of `quick.sh` for a machine where perf
lives on the host: it syncs, runs `make quick-legacy QUICK_BUILD_ONLY=1` (the old
and new libraries into `target/quick/`), then `bench/gce/quick-perf.sh` on
the VM host starts each (row, variant) detached in the container with
`sleep 1; exec env LD_PRELOAD=... <row>`, attaches `sudo perf stat -x,
--inherit -p <container init pid>` before the exec, waits for the
container, stops perf by killing its sleep child and prints cycles,
instructions, L1D and dTLB misses per operation (raw files in
`~/logs/quick-perf`). Extra `ROWS=`, `VARIANTS=`, `EVENTS=` arguments go to
that script; `OLD=`/`NEW=` to the build. `scorecard` runs `make scorecard`
(`bench/scorecard.sh`: the fixed row set under challoc and jemalloc, 3
rounds, one line appended to `target/results/scorecard-<arch>.md`) and then
`fetch`, so the appended line lands in the local file without clobbering a
newer local edit. Run it on both VMs after landing a change and commit the
two lines.

Every extra argument is a `VAR=value` for `make` and is quoted for the
remote shell, so spaces inside `VARIANTS` survive. Both subcommands `sync`
first; `sync` mirrors the working tree including uncommitted changes and
prints a warning listing any uncommitted change under `src/` (or the crate
manifest), because the library the VM then measures as `tree` is not HEAD.
Run `quick` before any `ab`; a variant that fails `quick` is not taken to
the protocol tier.

Long runs (`ab` with 5 rounds is 6 to 8 minutes, the soak 45) survive a dropped ssh session when launched detached on the VM:
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
with `EXTRA_DOCKER_FLAGS=--privileged`, so Docker's seccomp profile does not hide
`process_madvise` and `mseal`), fetches `~/logs` into
`target/results/matrix/<date>-<cell>.log` and deletes the VM, passed, failed,
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
(its log goes to `.tmp-matrix-lab/`, not `target/results/matrix/`); `matrix
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
fixed row set) written to `target/results/scorecard-<family>.md` on the VM
and fetched back next to the reference scorecards (`scorecard-aarch64.md`
and `scorecard-x86_64.md`, C4A and C4 at 16 vCPU), which it does not
touch; the per-CPU files are 8 vCPU, so compare them with the reference
CPU of the same architecture row by row (challoc-vs-jemalloc
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
`target/results/scorecard-<family>.md`.

## The Chainguard OS cells (Phase 2.5 task 14)

`challoc-bench matrix cgos` runs the `cgos-*` cells: the product's own
image (`kirkland-server-hypervisor-{arm64,amd64}`, project `wolfi-vm`; the
boot recipe, the metal shapes and the traps are in
`docs/runbooks/chainguard-os-gce.md`) on the reference shapes, so the host
is the kernel and glibc Chainguard ships (6.18 `-gcp`, Wolfi glibc 2.44),
not Ubuntu's. They are the "floor as shipped" cells and the first without
Docker between the library and the kernel: the probe line runs three ways,
inside podman with its default seccomp profile, inside podman
`--privileged`, and natively on the host (`libchalloc.so` and the
`features` binary copied out of the container, `LD_PRELOAD` on the host's
glibc, which is the same 2.44 the dev image has).

| cell       | image family                        | machine        | notes |
|------------|-------------------------------------|----------------|-------|
| cgos-arm64 | `kirkland-server-hypervisor-arm64` (`wolfi-vm`) | c4a-standard-8 | Axion; kernel 6.18.49-r2-gcp; THP `always` |
| cgos-amd64 | `kirkland-server-hypervisor-amd64` (`wolfi-vm`) | c4-standard-8  | Xeon 8581C; the same kernel; no `X86_5LEVEL`, no `ADDRESS_MASKING` |
| cgos72-amd64 | `kirkland-workstation-7dot2-amd64` (`wolfi-vm`) | c4-standard-8 | Xeon 8581C; kernel 7.2 (the workstation image, x86_64 only; added 2026-09-20 for the 7.2 rows of `docs/research.md`); scorecard file `scorecard-cgos72-amd64.md` |

A bootc host is provisioned differently (`matrix_provision_bootc_script`
in `challoc-bench`): `/usr` is a read-only composefs with no `apk` on the
host, so nothing is installed; `/var` and `$HOME` are one ext4 that the
image's first-boot growfs never grew (OS-3363: `resize2fs` is missing from
the image), so the provisioning grows it online with `resize2fs` from a
`wolfi-base` container that sees the mount (`-v /var:/mnt/var`; 4.9 GiB
to 49 GiB); Docker does not exist, rootless podman fails
(`/run/containers/storage` is root's) and netavark cannot build its bridge
(no `nft` on the host), so a `~/bin/docker` shim runs `sudo podman` with
`--network=host` and the Makefile is untouched; `make` and `getconf` are
copied out of a Wolfi container into `~/bin` (same glibc, they run
natively). The login is the gcloud user through the project's `ssh-keys`
metadata (no OS Login), and the cell runs as a transient systemd unit
(`sudo systemd-run --uid=...`): this image ends the session scope with the
ssh login, which killed a `setsid nohup` job. The cell script's `cgos`
mode (`matrix-cell.sh`) runs `make image`, `make test` under the default
seccomp profile and again with `EXTRA_DOCKER_FLAGS=--privileged`, the three
probe lines, the host capture (`host.log`: uname, `/proc/config.gz` keys,
THP, glibc, bootc status, the root mount, podman, the NIC driver), `make
test-cgroup` (podman sets the cgroup v2 limits) and one `make scorecard
ROUNDS=3` into `target/results/scorecard-<cellprefix>-<arch>.md` (`cgos-*` ->
`scorecard-cgos-<arch>.md`, `cgos72-amd64` -> `scorecard-cgos72-amd64.md`, so
the two kernels do not share a trend file), fetched back next to the
per-CPU files; the reference files are not touched. The summary
line gains `PROBE_NATIVE` and `privileged=PASS:<passed>/<failed>`.

```
bench/gce/challoc-bench matrix cgos                     # all three cells (6.18 arm64, 6.18 amd64, 7.2 amd64), throwaway VMs, concurrently
MATRIX_TIMEOUT=7200 MATRIX_ADOPT=challoc-lane-e-cgos-arm64 bench/gce/challoc-bench matrix cgos-arm64
                                                        # run the cell on an existing VM, keep it
```

`MATRIX_ADOPT=<vm>` runs a cell on a VM that already exists in any zone
(skipping create; the VM is provisioned and synced like a new one and never
deleted by the driver), for a pair created by hand from the runbook or a
`matrix lab` VM kept from before. A cell takes about 70 minutes on these
shapes (two `make test`, the 300 s oscillation, the scorecard), so pass
`MATRIX_TIMEOUT=7200`. The four-way table on these hosts
(`bench/results/2026-09-19-<arch>-cgos-four-way.md`) is not part of the
cell: it is the task 1 sequence (`make bench-build`, the two Ruby builds,
`make ab` with the five allocators, `make ruby-bench`, `make python-bench`,
`make test-cgroup-compare`) run on the adopted VM as a systemd unit, plus
the same rows natively and in the container (`bench/gce/native-rows.sh`, kept with the
results file's section on host-native versus in-container numbers).
Numbers from a `cgos-*` cell are 8 vCPU on a 6.18 kernel with THP
`always`: compare them with the same-shape `cpu-c4a` / `cpu-c4` cells and
the reference files by challoc-vs-jemalloc percentages per row, never by
absolute throughput.

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

## Files

- `challoc-bench`: the driver (this README's commands; `challoc-bench` with
  no argument prints its header).
- `quick-perf.sh`: `challoc-bench quick-perf`'s VM side, per-operation
  counters with `perf stat` attached from the host ("Two tiers on a VM").
- `perf-record.sh arm64|amd64 ROW VARIANT`: the `perf record` companion of
  `quick-perf.sh`: symbol shares of one row under one library, the DSOs
  copied out of the finished container for `perf report --symfs`; the
  attribution CLAUDE.md asks for before a fast-path change.
- `drun.sh`: the one `docker run` line the two scripts above share
  (the repository at `/src`, the architecture's
  target volume, `IMAGE`, default `challoc-dev:<arch>`).
- `matrix-cell.sh`: the VM side of `matrix` (the steps, the `MATRIX ...`
  summary line); `native-rows.sh`: the `ab.sh` rows run natively and in the
  container on a Chainguard OS host (the four-way tables' host-native
  section).
- `cpuid-dump.c`: the raw CPUID/HWCAP registers a CPU cell captures (the
  fixtures of `src/sys/cpu.rs`); `../tools/rt --probe` (was `pmu-probe.c`):
  does `perf_event_open` count on a host without a `perf` binary.
- In `bench/probes/`, used from a VM host beside a detached soak:
  `valkey-perf-sidecar.sh` (root: `perf stat -I 1000` for each server's
  life, 60 s `perf record` windows, `INFO commandstats` snapshots),
  `valkey-defrag-sidecar.sh` (no root: `loglevel verbose` so defrag's
  decision lines land in the server log, `INFO` sampled once a second);
  and two `LD_PRELOAD` sidecars built from source: `statsdump.c` (an
  `atexit` `malloc_stats_print` for a program that cannot call it, the JVM
  rows) and `arenasample.c` (per-second arena RSS attribution from
  `malloc_stats_print`).
