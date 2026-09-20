# Chainguard OS on arm64 metal: the bounded probe (Phase 2.5 task 14, lane E)

Two launches of `c4a-highmem-96-metal` in `us-central1-a` from
`kirkland-server-hypervisor-arm64` (project `wolfi-vm`), exactly the
runbook's command (`docs/runbooks/chainguard-os-gce.md` section 3:
`--maintenance-policy=terminate --boot-disk-type=hyperdisk-balanced
--boot-disk-size=200GB --metadata=serial-port-enable=TRUE`, no Shielded VM
flags), driven by a script on the Mac with a 55-minute watchdog that would
have deleted the instance whatever state the probe was in. The first
launch's native rows died on a script error (the row runner looked for a
probe's C source at the native root) after the probe itself had completed;
the instance was deleted and a second launch ran the rows. Raw log:
`bench/results/matrix/2026-09-19-cgos-metal-arm64.log` (both launches;
the console was never used, ssh only, as the runbook says).

| launch | created (UTC) | ssh up | probe done | delete issued | gone | lifetime |
|---|---|---|---|---|---|---|
| 1 | 2026-09-19T22:35:42Z | +66 s | 22:37:54Z | 22:38:21Z | 22:38:42Z (list empty) | 3 min |
| 2 | 2026-09-19T22:40:48Z | +120 s | 22:50:48Z | 22:51:25Z | 22:51:46Z (list empty) | 11 min |

Both well inside the 60-minute rule. The runbook's "8 to 15 minutes to
reach SSH" did not apply here: 66 and 120 seconds from `create` returning
to a working login (the hyperdisk boot image is 10 GB; the guest agent
had the ssh key from the project's metadata). `systemctl is-system-running`
still read `starting` at the time of the probe (1 to 2 minutes after
boot), with no failed units; the probe did not wait for `running`. The
guest clock was about five minutes ahead of UTC during the second run
(the row runner stamped 22:47:46Z at Mac time 22:42:44Z): no NTP sync yet
at that point of the boot.

## 1. What the host is

| check | result |
|---|---|
| kernel | `6.18.49-r2-gcp-6.18 #Chainguard SMP Fri Sep 11 18:10:11 UTC 2026 aarch64` (the same build as the VMs) |
| CPU | `CPU implementer 0x41, CPU part 0xd8e, variant 0x0, revision 0` on all 96 CPUs: **Neoverse-N3** (`lscpu`: `Model name: Neoverse-N3`, r0p0), 1 socket, 1 thread per core, 1 NUMA node |
| caches | L1d 64K, L1i 64K, L2 2048K per core (`/sys/devices/system/cpu/cpu0/cache`), L3 114688K (112 MiB, one instance); `lscpu` sums them as 6 MiB / 6 MiB / 192 MiB / 112 MiB |
| memory | 743 GiB total, 741 free |
| `nproc` | 96 |
| `systemd-detect-virt` | `google` (not `none`: systemd classifies the machine by its DMI vendor; the runbook expects `none`, see section 4) |
| `/sys/firmware/efi` | present: UEFI |
| NIC | `ethtool -i eth0`: driver `idpf`, version `6.18.49-r2-gcp-6.18` (the one check no VM can rehearse: passed) |
| root | `findmnt -no OPTIONS /`: `ro,relatime,lowerdir=/proc/self/fd/5::/proc/self/fd/6,index=off,verity=require` (fs-verity enforced); `bootc status --json` printed no `composefs=` token to grep on this build, so that check as written in the runbook is inconclusive here and `findmnt`'s `verity=require` is the evidence |
| `systemctl is-system-running` | `starting` at +1..2 min, `systemctl --failed` empty |
| `/dev/kvm` | present; `firecracker --version`: `Firecracker v1.17.0` (the runbook's arm64 acceptance) |
| glibc | `GNU C Library (glibc-2.44-r6) stable release version 2.44.` |
| podman | 6.1.2, cgroup v2 (root only, as on the VMs) |
| THP | `[always] madvise never`, 4 KiB pages |
| `perf_event_paranoid` | 2 |

Kernel config keys (from `/proc/config.gz`): `CONFIG_ARM64_MTE=y`,
`ARM64_PTR_AUTH=y`, `ARM64_BTI=y`, `ARM64_VA_BITS=52`, `ARM64_LSE_ATOMICS=y`,
`ARM64_PAN=y`, `ARM64_EPAN=y`, `ARM64_E0PD=y`, `ARM64_SVE=y`, `ARM64_SME=y`,
`RSEQ=y`, `SCHED_MM_CID=y`, `PERF_EVENTS=y`, `HW_PERF_EVENTS=y`, `ARM_PMU=y`,
`ARM_PMUV3=y`, `TRANSPARENT_HUGEPAGE=y`, `HZ=250`, `NUMA=y`, `KVM=y`,
`PREEMPT_NONE=y` (the arm64 build is `PREEMPT_NONE`; the x86_64 build of
the same kernel is `PREEMPT_DYNAMIC`/`PREEMPT_LAZY`, see the cells file).

## 2. MTE: no

`/proc/cpuinfo` `Features` on the N3: `fp asimd evtstrm aes pmull sha1 sha2
crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3
sm4 asimddp sha512 sve asimdfhm dit uscat ilrcpc flagm sb paca pacg dcpodp
sve2 sveaes svepmull svebitperm svesha3 svesm4 flagm2 frint svei8mm svebf16
i8mm bf16 dgh rng bti ...` and **no `mte`, no `mte3`** (the grep for
`mte[a-z0-9]*` in the line is empty). `dmesg` has no "Memory Tagging
Extension" line among the detected CPU features (it lists Address
authentication with the architected QARMA3 algorithm, Branch Target
Identification, E0PD, Enhanced PAN, HCRX_EL2, VHE, GICv3, ...), so the
kernel, built with `CONFIG_ARM64_MTE=y`, did not find MTE in ID_AA64PFR1_EL1
on this part. The native feature report agrees:

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

(`LD_PRELOAD` of the arm64 cell's `libchalloc.so`, built at `4864d4b`,
on the metal host's own glibc; run twice, once per launch, identical.)
So the arm64 metal shape is **not** the host plan task 6 (tagged pointers
part 2) is waiting for: neither Axion's Neoverse V2 in a VM nor Google's
Neoverse N3 on metal exposes MTE. It does expose PAC (used by task 7),
BTI, SVE/SVE2 and SME, LSE/LSE2, and a 52-bit VA.

## 3. The PMU: yes, natively and in a privileged container

`/sys/bus/event_source/devices/` lists `armv8_pmuv3_0` (cpus 0-95),
`arm_spe_0`, `arm_cmn_0` and the SMMUv3 PMCGs: the full PMU that no GCE
VM of the matrix has. There is no `perf` binary on the host and none in
Wolfi (`apk add perf` fails; the reference VMs use Ubuntu's
`linux-tools`), so the test is `bench/gce/pmu-probe.c` (`perf_event_open`
of `PERF_COUNT_HW_CPU_CYCLES` and `_INSTRUCTIONS`, user-only, around a
100 M-iteration loop):

| where | result |
|---|---|
| natively on the host | `pmu cycles=700599050 instructions=700000066 paranoid=2` |
| `sudo podman run` (default profile, default capabilities) | `cycles_open_errno=1(Operation not permitted)` for both counters (`perf_event_paranoid=2` plus the container's capability set: no `CAP_PERFMON`) |
| `sudo podman run --privileged` | `pmu cycles=700610829 instructions=700000066 paranoid=2` |

So a perf-driven attribution on Chainguard OS metal is possible with a
`perf` built into a container image (or Ubuntu's static perf copied over)
run `--privileged`, or natively with `perf_event_paranoid` lowered. On the
c4a-standard-8 VM the same probe returns `ENOENT` for both counters (no
PMU exposed), as expected.

## 4. Native rows on the N3 (challoc vs the four stock allocators)

The arm64 cell's binaries (`bench/rt`, larson, the Valkey model,
`libchalloc.so` at `4864d4b`, Wolfi's `libjemalloc.so.2` 5.3.1 and
`libtcmalloc_minimal.so.4` 2.18.1, `libmimalloc.so` v3.5.2 from
`bench/build.sh`) copied to the host; the same protocol as `bench/ab.sh`
(one warm-up, 3 interleaved timed rounds, sorted scores, `rt`'s peak RSS),
`rows.sh native`, 96 CPUs idle (load 0.46 at the start, 3.4 at the end).
These are the first challoc numbers on a Neoverse N3 and the first on
bare metal; 96 cores and a 112 MiB L3 make them incomparable in absolute
terms with the 8-vCPU cells, so read the percentages.

| program | challoc | glibc | jemalloc | mimalloc | tcmalloc | vs glibc | vs jemalloc | vs mimalloc | vs tcmalloc |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 (M ops/s) | 43.5 43.5 43.5 (13.6 MiB) | 39.7 (16.8) | 49.9 (26.3) | 30.9 (15.5) | 50.2 (13.1) | -8.7% | +14.7% | -29.0% | +15.4% |
| larson16 (M ops/s) | 593.4 596.5 596.8 (150.2 MiB) | 456.0 (123.4) | 673.6 (252.7) | 465.4 (119.5) | 636.0 (115.1) | -23.6% | +12.9% | -22.0% | +6.6% |
| valkey1 (M ops/s, write phase) | 12.549 13.318 13.346 (584.6 MiB) | 6.802 (568.6) | 14.139 (610.4) | 3.753 (585.0) | 12.755 (589.5) | -48.9% | +6.2% | -71.8% | -4.2% |

(Positive = challoc worse. Every allocator's library was confirmed mapped
in `/proc/self/maps` before the rounds.) The N3 reads like the Altra
rows of the CPU matrix (`2026-09-19-altra-larson-attribution.md`) more
than like Axion: challoc trails jemalloc by 13..15% on larson and by 6% on
the Valkey model where the c4a-standard-8 Chainguard OS cell has +8%,
+5% and +6% (`scorecard-cgos-arm64.md`), and the 16-thread row's RSS
under challoc is 150 MiB against jemalloc's 253 and tcmalloc's 115 (the
retention-until-decay mechanism M3 of the four-way attribution; THP
`always` on this host, see the cells file for the AnonHugePages
measurements). One larson1 round and one valkey1 round were 6% below the
other two under challoc; nothing else ran on the machine.

## 5. Runbook acceptance checks, as run

`ls /sys/firmware/efi` exists; `systemd-detect-virt` prints `google`
(not `none`); `nproc` 96; `ethtool -i eth0` driver `idpf`; `bootc status
--json | grep -o 'composefs=...'` prints nothing on this build (the
`findmnt` line shows `verity=require`); `findmnt -no OPTIONS /` is the
overlay with `verity=require`; `systemctl is-system-running` was
`starting` with zero failed units at 1..2 minutes; `/dev/kvm` exists and
`firecracker --version` is 1.17.0 (`lsmod | grep kvm` printed nothing:
KVM is built in, `CONFIG_KVM=y`). Nested virtualization was not tested
(OS-3364 says the N3 lacks FEAT_NV2).
