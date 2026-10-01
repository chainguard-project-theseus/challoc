# Runbook: boot Chainguard OS on GCE (VMs and bare metal, x86_64 and arm64)

Received from the human on 2026-09-19 (their runbook, verified against the
live `wolfi-vm` project that day; where something is reported rather than
re-checked, it says so), with a challoc-specific preamble. It exists so
that any session can put challoc on the product's own kernel and glibc,
outside Docker, on a VM for cents or on metal for the PMU and for CPU
features a hypervisor hides.

## Why challoc cares

- The kernel matrix (`bench/gce/challoc-bench matrix`) runs Debian and Ubuntu kernels and tests challoc inside the
  Wolfi dev container. Chainguard OS is the kernel and glibc the product
  ships; a Chainguard OS host is the first "floor as shipped" cell, and
  the only one without Docker's default seccomp hiding `mseal` and
  `process_madvise`.
- Metal shapes have the full PMU (`c3-standard-192-metal` was used once,
  see the memory note "PMU on GCE means bare metal") and no hypervisor to
  hide CPU features: the arm64 metal shape measures as Neoverse **N3**
  (Armv9.2), so `grep mte /proc/cpuinfo` there is the cheapest test of
  whether the tagged-pointer implementation (design section 18) has a
  host.
- Our VMs live in the active gcloud project (`dlorenc-dev`); the images
  live in `wolfi-vm`. Pass `--image-project=wolfi-vm` and keep
  `CHALLOC_GCE_PROJECT` unset; `gcloud compute images describe-from-family
  kirkland-server-hypervisor-amd64 --project=wolfi-vm` works from this
  account (checked 2026-09-19).
- Chainguard OS is a bootc/composefs system with a read-only, fs-verity
  root. Expect `apk add` on `/usr` to fail until `bootc usroverlay` (a
  transient writable `/usr` for the session) is run; `/var` and `$HOME`
  are writable. Wolfi's glibc 2.44 is the host glibc, so binaries built
  in the dev container on a reference VM run natively on this host.
- Shapes seen in `us-east4-a` on 2026-09-19: `c3-standard-192-metal`,
  `c4-standard-288-metal`, `c4-highmem-288-metal` (and `-lssd-` variants);
  `c4a-highmem-96-metal` is in `us-central1-a/b/c`, `us-east4-c`,
  `us-east5-b`, `us-west4-a/c`, not `us-east4-a`.
- Every VM this repo starts is stopped or deleted before the iteration
  ends; metal is about $10/h, so delete it within the hour.
- Kernel 7.2 images (the human, 2026-09-20; verified with `gcloud compute
  images describe-from-family`): `kirkland-workstation-7dot2-amd64` for
  VMs (guest features UEFI/GVNIC/IDPF, no `BARE_METAL_LINUX_COMPATIBLE`)
  and `kirkland-workstation-7dot2-metal-amd64` for `*-metal` shapes, both
  in `wolfi-vm`, x86_64 only. These are the first hosts newer than GCE's
  Ubuntu 7.0: use them for the 7.2 items in `docs/research.md` (khugepaged
  anon mTHP collapse, cache-aware scheduling). A
  `cgos72-amd64` cell should be a copy of `cgos-amd64` with the family
  swapped (`bench/gce/challoc-bench`, the cell table).

What the first cells found (lane E, 2026-09-19; `bench/gce/challoc-bench`
`matrix cgos`, `bench/results/2026-09-19-chainguard-os-cells.md`), where
it adds to or corrects the runbook below:

- The 50 GB boot disk does not give you 50 GB: OS-3363 is live in the
  2026-09-16 images. `/var` and `/home` are one ext4 of 4.9 GiB on a 49 GiB
  partition (`lsblk` shows the partition grown, `df` does not). The fix
  needs no reboot and no image change: `sudo podman run --rm --privileged
  --network=host -v /dev:/dev -v /var:/mnt/var cgr.dev/chainguard/wolfi-base
  sh -c 'apk add e2fsprogs-extra; resize2fs /dev/nvme0n1p2'` (p3 on the
  amd64 image; `findmnt -no SOURCE /var` names it) grows the mounted
  filesystem online in two seconds. The `-v /var` mount is what makes
  resize2fs see the filesystem as mounted; without it, it opens the device
  offline and fails with "Device or resource busy".
- podman is root-only here: rootless fails with `/run/containers/storage:
  permission denied`. Use `sudo podman`. Its default seccomp profile is
  `/usr/share/containers/seccomp.json`, cgroup v2 with the systemd manager,
  `--memory` limits work (`memory.max` reads the limit inside).
- podman's default bridge network cannot come up: netavark needs `nft` and
  the host has none (`iptables` is present, but netavark 1.x uses nftables).
  Every `podman run` and `podman build` needs `--network=host` (or a
  `containers.conf` netns setting). Host networking also sidesteps the GCE
  1460-byte MTU that Docker needed configuring for.
- The host has no `make`, `getconf`, `gcc`, `git`, `python3`, `perf`,
  `ldd`, `lscpu`, `nft`; it has `bash`, `curl`, `rsync`, `jq`, `flock`,
  `setsid`, `ethtool`, `iptables`, `mkfs.ext4` (but not `resize2fs`),
  `libstdc++.so.6`, and `getenforce` (SELinux is not enabled; no
  `/sys/fs/selinux`). Binaries copied out of a Wolfi container run
  natively (glibc 2.44-r6 on the host), which is how the cell gets `make`
  and `getconf`, and how challoc's `features` probe, larson and the Valkey
  model ran on the host.
- A detached job started over ssh with `setsid nohup ... &` dies when the
  ssh session ends (the session scope is stopped; no `KillUserProcesses`
  line is set in `logind.conf`, so this is the image's default). Use a
  transient system unit: `sudo systemd-run --collect --unit=NAME
  --uid=$(id -u) --gid=$(id -g) --setenv=HOME=$HOME --working-directory=DIR
  bash -c '...'`; `loginctl show-user` reports `Linger=no`, so a `--user`
  unit would die too.
- `gcloud compute ssh` logs in as the local user through the project's
  `ssh-keys` metadata (no OS Login on this image; the instance carries no
  metadata). Direct `ssh -i ~/.ssh/google_compute_engine <user>@<ip>` and
  rsync work. A VM recreated under an old IP changes its host key.
- `/dev/kvm` is absent on the C4A and C4 VMs (no nested virtualization on
  these shapes), so nothing here exercised Firecracker; the runbook's
  x86 pre-flight uses N2 with `--enable-nested-virtualization` for that.
- The kernel is `6.18.49-r2-gcp-6.18` on both architectures (2026-09-16
  images): `CONFIG_RSEQ`, `SCHED_MM_CID`, `PSI`, `MEMCG`, `PERF_EVENTS`,
  HZ 250; arm64 `ARM64_MTE`, `ARM64_PTR_AUTH`, `ARM64_BTI`, 52 VA bits, 4K
  pages; x86 without `X86_5LEVEL` and without `ADDRESS_MASKING` (LAM), so
  even a LAM-capable CPU could not be used from this kernel. THP is
  `always` (2 MiB `inherit`, every mTHP size present and `never`).
  `perf_event_paranoid` is 2.
- The kernel 7.2 workstation image (`kirkland-workstation-7dot2-amd64-
  20260920-0028`, measured 2026-09-20 by the `cgos72-amd64` cell,
  `bench/results/2026-09-20-chainguard-os-cells.md`): kernel
  `7.2.3-r0-desktop-7.2 #Chainguard SMP PREEMPT_DYNAMIC` (`sched/preempt`
  `(full) lazy`), glibc 2.44-r6, THP **`madvise`** (2 MiB `inherit`,
  16kB..1024kB `never`), `khugepaged` defaults (`max_ptes_none 511`,
  `pages_to_scan 4096`, `scan_sleep 10 s`), `perf_event_paranoid` 2, no
  PMU. **No `/proc/config.gz`** and no config file under `/boot` or
  `/usr/lib/modules`, so the option bits are read from `kallsyms` and
  debugfs: `RSEQ` and `SCHED_MM_CID` (`mm_cid_*` symbols, `rseq_size=33
  mm_cid=yes` in the feature line), **`RSEQ_SLICE_EXTENSION` on**
  (`/sys/kernel/debug/rseq/{debug=0,slice_ext_nsec=5000}`, `T
  __x64_sys_rseq_slice_yield`), `SCHED_CACHE`
  (`/sys/kernel/debug/sched/llc_balancing/`, `enabled=1`),
  `ANON_VMA_NAME` (`anon_name=yes`, unlike the 6.18 server kernel), the
  per-size `hugepages-<size>kB/stats/collapse_*` counters. Other
  differences from the server image: **OS Login only** (the guest agent
  creates no user from `ssh-keys` metadata; `enable-oslogin=TRUE` on the
  instance and the OS Login posix user, with `sudo` and the `docker`
  group, log in), **Docker 29.8.1 with `dockerd` running** plus `make`,
  `gcc`, `git` on the host (still no `perf`, `nft`, `lscpu`; rootless
  podman still fails), `/var` 19 GiB as shipped (the cell grows it),
  NIC `gve`, `systemd-detect-virt` = `google`.

---

The human's runbook follows, verbatim apart from formatting.

**Read the "Things that will mislead you" section before you debug
anything.** Three of the four blockers this cell hit cost hours because
the symptom pointed the wrong way.

## 0. What you need

* `gcloud` authenticated, project `wolfi-vm` (for the images; the
  instances can live in another project)
* Quota is not a constraint: `C3-CPUS = 500`/region, and C4A needs no
  allowlist (an earlier claim that it did was wrong, see OS-3311)
* A card for ~$10/hr. **Terminate when done.**

## 1. Pick an image

Both arches are published and ready **today**:

| Arch | Image family | Latest verified |
|--|--|--|
| x86_64 | `kirkland-server-hypervisor-amd64` | `kirkland-server-hypervisor-amd64-20260916-1603` |
| arm64 | `kirkland-server-hypervisor-arm64` | `kirkland-server-hypervisor-arm64-20260916-1358` |
| x86_64, kernel 7.2 (VM; added 2026-09-20, lane E) | `kirkland-workstation-7dot2-amd64` | `kirkland-workstation-7dot2-amd64-20260920-0028` (created 2026-09-20 00:50 UTC; UEFI/GVNIC/IDPF, **no** `BARE_METAL_LINUX_COMPATIBLE`, so VMs only; the `cgos72-amd64` cell) |
| x86_64, kernel 7.2 (metal; added 2026-09-20, lane E) | `kirkland-workstation-7dot2-metal-amd64` | `kirkland-workstation-7dot2-metal-amd64-20260919-2048` (created 2026-09-20 02:11 UTC; carries `BARE_METAL_LINUX_COMPATIBLE`) |

Both carry the guest OS features metal requires, confirmed with `gcloud
compute images describe`:

```
UEFI_COMPATIBLE;VIRTIO_SCSI_MULTIQUEUE;GVNIC;IDPF;BARE_METAL_LINUX_COMPATIBLE
```

`BARE_METAL_LINUX_COMPATIBLE` **is mandatory.** Without it GCE refuses to
boot a `*-metal` instance from the image at all. It is opt-in:
`tools/google-image-upload` only adds it when passed `--bare-metal`,
which the Makefile does for this config.

Note on the `kirkland-` prefix: image names are built from `PREFIX != id
-un` in wolfi-vm's Makefile, i.e. the username of whoever ran the
publish. If you build your own they will be
`<you>-server-hypervisor-<arch>-<timestamp>`. Nothing about the image is
user-specific; only the name is.

## 2. Do the cheap pre-flight first

A metal launch is ~$10/hr and takes 8 to 15 minutes to reach SSH. A VM
costs cents and proves everything except the metal-specific behaviour.

```bash
# x86_64: nested virt gives you /dev/kvm to test Firecracker
gcloud compute instances create cg-hypervisor-vm-x86 \
  --project=wolfi-vm --zone=us-central1-a \
  --machine-type=n2-standard-8 \
  --enable-nested-virtualization \
  --image-family=kirkland-server-hypervisor-amd64 --image-project=wolfi-vm

# arm64
gcloud compute instances create cg-hypervisor-vm-arm \
  --project=wolfi-vm --zone=us-central1-a \
  --machine-type=c4a-standard-4 \
  --image-family=kirkland-server-hypervisor-arm64 --image-project=wolfi-vm
```

(For challoc: same commands with `--project` omitted or set to the
active project, a `challoc-<lane>-cgos-<arch>` name, zone `us-east4-a`,
and the reference shapes `c4-standard-8` / `c4a-standard-8` so the
numbers are comparable with the CPU matrix.)

This exercises systemd-boot, the composefs deployment, fs-verity,
`google-guest-agent` and (x86) `/dev/kvm` + Firecracker. **It cannot
exercise IDPF**: C4 VMs require gVNIC and reject `nic-type=IDPF`, so the
NIC driver is only ever tested on real metal.

The arm64 VM additionally proves something metal cannot: it reports
`Secure Boot: enabled (user)` with active TPM2 PCR banks, against our own
enrolled keys.

## 3. Launch metal

Both shapes are available in `us-central1-a` (verified). c4a is also in
us-central1-b/c, us-east4-c, us-east5-b, us-west4-a/c.

### x86_64: `c3-standard-192-metal` (192 vCPU / 768 GB advertised, 755 GB visible)

```bash
gcloud compute instances create cg-hypervisor-metal-x86 \
  --project=wolfi-vm --zone=us-central1-a \
  --machine-type=c3-standard-192-metal \
  --maintenance-policy=terminate \
  --boot-disk-type=hyperdisk-balanced --boot-disk-size=200GB \
  --image-family=kirkland-server-hypervisor-amd64 --image-project=wolfi-vm \
  --metadata=serial-port-enable=TRUE
```

### arm64: `c4a-highmem-96-metal` (96 vCPU / 768 GB, Axion)

```bash
gcloud compute instances create cg-hypervisor-metal-arm \
  --project=wolfi-vm --zone=us-central1-a \
  --machine-type=c4a-highmem-96-metal \
  --maintenance-policy=terminate \
  --boot-disk-type=hyperdisk-balanced --boot-disk-size=200GB \
  --image-family=kirkland-server-hypervisor-arm64 --image-project=wolfi-vm \
  --metadata=serial-port-enable=TRUE
```

### Why each flag is there

| Flag | Why |
|--|--|
| `--maintenance-policy=terminate` | **Mandatory.** Metal cannot live-migrate; the API rejects the default. |
| `--boot-disk-type=hyperdisk-balanced` | **Mandatory.** `us-central1-a` offers no `pd-*` types for metal. A `pd-balanced` default is the most common launch failure. |
| `--boot-disk-size=200GB` | The image is 10 GB. See the growfs caveat below before assuming you get 200 GB of usable root. |
| `--metadata=serial-port-enable=TRUE` | Lets you capture first boot. Worth it on a shape that takes minutes to appear. |
| no Shielded VM flags | C3 metal does not support Shielded VM; passing them fails the insert. On C4A the API **accepts** `--shielded-secure-boot` and the firmware silently ignores it (OS-3313), which is worse than rejecting it. |

## 4. Wait for SSH; do not judge by the console

```bash
gcloud compute ssh cg-hypervisor-metal-x86 --project=wolfi-vm --zone=us-central1-a
```

**Metal takes 8 to 15 minutes to reach SSH.** That is firmware and
provisioning, not a hang.

## 5. Things that will mislead you

**The serial console goes silent, and the machine is fine.** After
`legacy bootconsole [earlyser0] disabled` you get nothing more. That is
the designed handoff from `earlyprintk` to `console=tty0` while
`8250.nr_uarts=0` works around OS-3347. It is not a hang. **Confirm a
metal boot over SSH, never by watching the console.**

**If you do read the console, it is 115200.** GCE's metal serial console
runs at 115200; a bare `console=ttyS0` leaves the kernel at the 8250
default of 9600, and the result is not garbled output, it is *no* output.
A running machine looks identical to a dead one. This cost hours and
produced a wrong conclusion that metal "would not boot".

**Ignore anything that says this image needs GRUB.** It does not.
OS-3310 still describes a UEFI-GRUB workaround; that was retracted
(OS-3330). Every systemd-boot metal attempt predated both the 115200
discovery and the `univ8250_console_init` hang, and SSH was never
checked, so a silent-but-working systemd-boot was indistinguishable from
one that never ran. GCE is UEFI on every shape including metal. If you
are tempted to add a bootloader to fix a metal boot, **check the console
baud and check SSH first.**

## 6. Acceptance checks

```bash
ls /sys/firmware/efi            # exists -> UEFI on real metal
systemd-detect-virt             # none   -> genuinely bare metal
nproc                           # 192 (c3) or 96 (c4a)
ethtool -i $(ip -o route get 8.8.8.8 | awk '{print $5}') | grep driver
                                # idpf   -> the one thing no VM can rehearse
bootc status --json | grep -o 'composefs=[^ "]*'
                                # NO leading '?' -> fs-verity enforced
findmnt -no OPTIONS /           # overlay ro,...,verity=require
systemctl is-system-running     # running, zero failed units
```

**x86 only**, nested virt:

```bash
ls /dev/kvm && lsmod | grep kvm_intel
firecracker --version
```

**arm64**: the same two commands, but expect `kvm` rather than `kvm_intel`:

```bash
ls /dev/kvm && lsmod | grep kvm
firecracker --version
```

The arm64 metal host **does** have a working `/dev/kvm` and runs
Firecracker: 384 concurrent microVMs have been booted on
`c4a-highmem-96-metal` with zero failures. What is absent on arm64 is
**nested** virtualization: a guest running on this host cannot itself be
a KVM host. That is expected, not a fault. C4A metal is Neoverse **N3**,
which does not implement FEAT_NV2. Confirmed by booting with
`kvm-arm.mode=nested` and reading the kernel's own verdict: `CPU
features: unavailable: Nested Virtualization Support` (OS-3364). Google
documents C4A as Neoverse V2; the metal shape measures as N3. Worth
knowing before you target `-mcpu=neoverse-v2`.

## 7. Scratch for microVMs

The `-gcp` subpackage provisions `/var/lib/microvm-scratch` from a
Hyperdisk you attach under a known device name, or Local SSD, in
`SCRATCH_ORDER` preference. **Hyperdisk is tried first**: Local SSD does
not exist at all on `c3-*-metal` or `c4a-highmem-96-metal`, and Hyperdisk
survives stop/start.

```bash
gcloud compute instances attach-disk cg-hypervisor-metal-x86 \
  --project=wolfi-vm --zone=us-central1-a \
  --disk=my-scratch-disk --device-name=microvm-scratch
```

Then reboot and check the journal. With no scratch attached the correct
behaviour is to touch nothing: the provisioner refuses the boot disk by
its `nvme_card-pd` model, which you can see it doing in the log.

## 8. Known issues (read before filing a bug)

| Issue | Impact |
|--|--|
| OS-3363, root ext4 never grows | You ask for a 200 GB boot disk and the root filesystem stays small. Probable cause found 2026-09-19: `resize2fs` lives in `e2fsprogs-extra`, not `e2fsprogs`, so it was missing from the image and first-boot growfs could not run. Fix in flight in stereo#336983. Images published before that merges still have it. |
| OS-3347, `8250.nr_uarts=0` | A workaround, not a fix. Costs the serial console on metal, which is the channel you need when SSH is down. |
| OS-3016, no verified boot on x86 metal | C3 metal reports `Secure Boot: disabled (unsupported)`. The enrolled certs are inert there. The same image enforces Secure Boot correctly on a VM. |
| OS-3313, C4A accepts and ignores Secure Boot | The API takes `--shielded-secure-boot`, the firmware does nothing with it. Do not read a successful launch as verified boot. |
| OS-3364, no nested virt on arm64 | Hardware limit (N3 lacks FEAT_NV2), not config. Any workload needing melange's qemu runner has to land on x86 hosts. |

## 9. Cost

`c3-standard-192-metal` is roughly **$9.68/hr** on-demand (figure from
OS-3310, not re-quoted). A couple of hours of bring-up is ~$20. **An
instance left running overnight is ~$230.**

```bash
gcloud compute instances delete cg-hypervisor-metal-x86 \
  --project=wolfi-vm --zone=us-central1-a --quiet
```

## 10. Building your own image

Only needed if you are changing the OS rather than using it. Full detail
in `configs/gcp-server-hypervisor/README` in wolfi-vm.

```bash
cd wolfi-vm
make disk-gcp-server-hypervisor                      # x86_64
ARCH=aarch64 make disk-gcp-server-hypervisor         # arm64, needs an aarch64 builder
make publish-gcp-server-hypervisor
```

`ARCH=aarch64` wants a real aarch64 builder: `bootc install` cannot cross
architectures, since it uses the running system's own bootctl and
partition logic rather than the target image's. On an x86_64 host it runs
under TCG at ~20 to 30 min instead of ~5.

The disk is **not** assembled from an apko package list. It is produced
by installing the published bootable container image
`cgr.dev/chainguard-private/chainguard-server-hypervisor-gcp` with
`apkoaas container-installer`, which hands the ref to `bootc install
--target-imgref` so the installed system knows where to `bootc upgrade`
from.

## Open PRs behind this (at the time of writing)

The GCP path is green in CI but not fully merged: chainguard-dev/stereo
#336983 (dual-arch GCP bootc image, includes the `resize2fs` fix) and
chainguard-dev/wolfi-vm #1617 (the GCE disk/image config). Already
merged: stereo #332213 (GCE platform subpackage), stereo #340373
(guest-kernel KVM modules).
