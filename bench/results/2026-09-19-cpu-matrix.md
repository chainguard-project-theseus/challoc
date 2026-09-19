# The CPU matrix on GCP (Phase 2.5 task 4)

Date 2026-09-19 (10:17..11:25 UTC). Tree 7b2c50b (the driver and the `uai`
probe; every cell ran `make test` and its scorecard at this commit), then
24d4130 (the `la57_enabled` test fix and the captured fixtures; the C3D
`make test` rerun and the two attribution labs ran at it). Driver
`bench/gce/challoc-bench matrix cpu` (`bench/gce/README.md`, "The CPU
matrix"), VM side `bench/gce/matrix-cell.sh` with the `scorecard` flag;
eight throwaway `-standard-8` VMs, three at a time, one per machine
family, all on `ubuntu-2404-noble-amd64/arm64-v20260918` and its
`7.0.0-1011-gcp` kernel, so only the CPU varies; every VM deleted by the
driver (the reference VMs stayed TERMINATED). Per-cell logs
`bench/results/matrix/2026-09-19-cpu-<family>.log` (driver lines, the VM's
`matrix.log`, the full `cpuinfo.log` with `/proc/cpuinfo`, `lscpu` and the
raw `cpuid`/HWCAP registers of `bench/gce/cpuid-dump.c`, and the tail of
every step log); the two lab VMs' logs `2026-09-19-cpu-{c3,c3d}-lab.log`.
Per cell: `make image`, `make test`, the probe line (`tests/c/features.c`
under `LD_PRELOAD`, default seccomp and `--privileged`), the CPU capture,
and one `make scorecard ROUNDS=3` (challoc versus jemalloc, the fixed row
set, 3 interleaved timed rounds plus a warm-up) appended to
`bench/results/scorecard-<family>.md`, a new file per CPU; the reference
files `scorecard-aarch64.md` and `scorecard-x86_64.md` were not touched.
The cells are 8 vCPU where the reference scorecards are 16, so absolute
throughput is not compared with the reference files; the comparison is
challoc-vs-jemalloc per row per CPU against the same row on the reference
CPU of the architecture (the 3ed77b7 lines, the latest in both files, the
last commit before this one that touched a fast path). The 8-vCPU C4A and
C4 cells are the same-shape controls.

## 1. The three answers the phase wanted

- **No GCP CPU of the matrix exposes MTE.** `mte=no mte3=no` on the C4A
  (Axion, Neoverse V2, `hwcap2=0x3f3ff`) and the T2A (Ampere Altra,
  Neoverse N1, `hwcap2=0`; Altra is Armv8.2 and has no MTE in hardware).
  Tagged pointers on MTE (task 6) still need another box (AmpereOne,
  Graviton4, or an Axion host that exposes it).
- **No GCP CPU of the matrix exposes LAM.** `lam=no` on every x86 cell:
  leaf 7 subleaf 1 `EAX` is `0x1c30` on the 8581C (C4, N4) and the 8481C
  (C3), `0x20` on Genoa, absent (no subleaf 1) on Milan and Cascade Lake.
  Emerald and Sapphire Rapids predate LAM; the first server parts with it
  are Granite Rapids and Sierra Forest, which no GCE family of this matrix
  runs.
- **Genoa does not expose UAI to the guest.** The new `uai` probe (`cpuid`
  `0x80000021` `EAX[7]`, `src/sys/cpu.rs`) reads `0x10000067` on the C3D's
  EPYC 9B14: bits 0, 1, 2, 5, 6 and 28 set, bit 7 clear, `uai=no`. Zen 4
  has UAI in hardware, so the hypervisor's CPUID hides it (it also hides
  LA57 on the same guest, see the feature table). Milan (N2D, EPYC 7B13)
  reads `0x10000020`, no UAI, as Zen 3 has none. Intel's extended range
  ends at `0x80000008`, so the leaf is not read there (`uai=no`).

Consequence for task 5: no CPU reachable on GCE runs tagged pointers in
hardware, on either architecture; the design section can only be written
against the manuals, and part 2 (task 6) needs a non-GCE host or a GCE
bare-metal shape (`c3-standard-192-metal` would show Sapphire Rapids'
real CPUID, still without LAM; an AMD `-metal` shape would show UAI).

## 2. The cells

| cell | machine | zone | CPU (model name / lscpu) | vCPU | kernel | make test | scorecard | wall | outcome |
|---|---|---|---|---|---|---|---|---|---|
| cpu-c4a | c4a-standard-8 | us-east4-a | Neoverse-V2 (Google Axion, Neoverse V2 (MIDR 0x410fd4f1)) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 758 s | PASS |
| cpu-t2a | t2a-standard-8 | us-central1-a | Neoverse-N1 (Ampere Altra, Neoverse N1 (MIDR 0x413fd0c1)) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 970 s | PASS |
| cpu-c4 | c4-standard-8 | us-east4-a | INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz (Emerald Rapids) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 817 s | PASS |
| cpu-c3 | c3-standard-8 | us-east4-a | Intel(R) Xeon(R) Platinum 8481C CPU @ 2.70GHz (Sapphire Rapids) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 877 s | PASS |
| cpu-c3d | c3d-standard-8 | us-east4-a | AMD EPYC 9B14 (Genoa, Zen 4) | 8 | 7.0.0-1011-gcp | 334/1 at 7b2c50b (`probe_matches_proc_cpuinfo`, section 5), **467/0 at 24d4130** on the lab VM | PASS | 813 s | FAIL at 7b2c50b, PASS at 24d4130 |
| cpu-n2d | n2d-standard-8 | us-east4-a | AMD EPYC 7B13 (Milan, Zen 3) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 908 s | PASS |
| cpu-c2 | c2-standard-8 | us-east4-a | Intel(R) Xeon(R) CPU @ 3.10GHz (Cascade Lake) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 951 s | PASS |
| cpu-n4 | n4-standard-8 | us-east4-a | INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.10GHz (Emerald Rapids on the N4 platform) | 8 | 7.0.0-1011-gcp | 465/0 | PASS | 842 s | PASS |

No cell was PENDING: every family had capacity in its first zone
(`us-east4-a`; T2A in `us-central1-a`). N2D gave Milan (EPYC 7B13), not
Rome. N4 exists in `us-east4-a` and gave the same 8581C as C4 at a lower
clock (2.10 against 2.30 GHz). The scorecard on 8 vCPU took 8.5..11.5
minutes of the 12.5..16 minute cells.

## 3. The feature lines, verbatim (`challoc_features()`, default seccomp)

| cell | features |
|---|---|
| cpu-c4a | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M` |
| cpu-t2a | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes mte=no mte3=no pac=no lse=yes lse2=no bti=no sve=no cacheline=64 l1d=64K l2=1M` |
| cpu-c4 | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M` |
| cpu-c3 | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M` |
| cpu-c3d | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=no la57_enabled=no cldemote=no waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=32K l2=1M` |
| cpu-n2d | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=no la57_enabled=no cldemote=no waitpkg=no erms=yes fsrm=yes avx512f=no cacheline=64 l1d=32K l2=512K` |
| cpu-c2 | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=no la57_enabled=no cldemote=no waitpkg=no erms=yes fsrm=no avx512f=yes cacheline=64 l1d=32K l2=1M` |
| cpu-n4 | `page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M` |

Under `--privileged` every cell reads `mseal=yes process_madvise=pidfd_self`
with the same CPU half (the kernel is the same 7.0.0-1011-gcp everywhere;
Docker's default seccomp profile hides the two syscalls, as on the
reference VMs).

## 4. The feature table across CPUs

arm64 (`AT_HWCAP`/`AT_HWCAP2`, `CTR_EL0`):

| feature | cpu-c4a (Neoverse-V2) | cpu-t2a (Neoverse-N1) |
|---|---|---|
| mte | no | no |
| mte3 | no | no |
| pac | yes | no |
| lse | yes | yes |
| lse2 | yes | no |
| bti | yes | no |
| sve | yes | no |
| cacheline | 64 | 64 |
| l1d | 64K | 64K |
| l2 | 2M | 1M |
| HWCAP, HWCAP2 | `0xefffffff`, `0x3f3ff` | `0x119fff`, `0x0` |

x86_64 (`cpuid`):

| feature | cpu-c4 (Emerald Rapids) | cpu-n4 (Emerald Rapids on the N4 platform) | cpu-c3 (Sapphire Rapids) | cpu-c3d (Genoa) | cpu-n2d (Milan) | cpu-c2 (Cascade Lake) |
|---|---|---|---|---|---|---|
| lam | no | no | no | no | no | no |
| uai | no | no | no | no | no | no |
| la57 | yes | yes | yes | no | no | no |
| la57_enabled | yes | yes | yes | no | no | no |
| cldemote | yes | yes | yes | no | no | no |
| waitpkg | no | no | no | no | no | no |
| erms | yes | yes | yes | yes | yes | yes |
| fsrm | yes | yes | yes | yes | yes | no |
| avx512f | yes | yes | yes | yes | no | yes |
| cacheline | 64 | 64 | 64 | 64 | 64 | 64 |
| l1d | 48K | 48K | 48K | 32K | 32K | 32K |
| l2 | 2M | 2M | 2M | 1M | 512K | 1M |
| max leaf, max ext. leaf | `0x00000020`, `0x80000008` | `0x00000020`, `0x80000008` | `0x0000001f`, `0x80000008` | `0x00000010`, `0x80000021` | `0x0000000d`, `0x80000021` | `0x0000000d`, `0x80000008` |
| leaf 0x80000021 EAX | absent | absent | absent | `0x10000067` | `0x10000020` | absent |

Readings. The 8581C (C4 and N4) and the 8481C (C3) report the same
feature half except that the 8481C has HLE (`ebx` bit 4) clear, irrelevant
here. WAITPKG is hidden by the hypervisor on all three (Sapphire and
Emerald Rapids have it). Genoa's guest CPUID hides LA57 (leaf 7 `ECX[16]`
clear) while `/proc/cpuinfo` still prints `address sizes: 52 bits physical,
57 bits virtual` from `cpuid 0x80000008` (`EAX = 0x3934`), so the kernel
runs four-level paging and the hinted `mmap` lands below 2^47
(`la57_enabled=no`); this is what broke the container test on the C3D cell
(section 5). Milan has no leaf 7 subleaf 1, no AVX-512 and a 512 KiB L2;
Cascade Lake has no FSRM, no LA57 and a 1 MiB L2; Genoa a 1 MiB L2; the
Intel server parts 2 MiB. Altra has LSE and nothing else the report looks
at (no PAC, LSE2, BTI, SVE), a 1 MiB L2. Every CPU reports a 64-byte
line. CLDEMOTE is exposed only on the Intel Sapphire/Emerald Rapids cells.

## 5. The one test failure and its fix

`sys::cpu::linux_tests::probe_matches_proc_cpuinfo` failed on the C3D
cell at 7b2c50b (`make test` stopped after the unit tests: 334/1), on the
assertion that `la57_enabled` equals `/proc/cpuinfo`'s `57 bits virtual`.
The line is the CPU's virtual address width, not the paging mode; without
the LA57 feature bit the kernel cannot run five-level paging whatever the
width says. The test (24d4130) applies the check only when `la57=yes` and
asserts `la57_enabled=no` without it. The library was right; the test's
reading of `/proc/cpuinfo` was wrong. Rerun on a C3D lab VM at 24d4130:
337 + 107 + 23 = 467/0, the probe line unchanged (`cpu-c3d-lab.log`). The
composed Altra and Zen 4 fixtures of task 2 are replaced by the captures
(T2A `hwcap=0x119fff`; Genoa leaf 7 `eax=1 ebx=0xf1bf07ab ecx=0x00405f46
edx=0x10`, 7.1 `eax=0x20`, `0x80000021 eax=0x10000067`, the `0x8000001D`
cache leaves; Milan leaf 7 `ebx=0x219c07ab ecx=0x00400604 edx=0x10`,
`0x80000021 eax=0x10000020`); host tests 267.

## 6. The scorecard across CPUs (challoc versus jemalloc, percent, positive = challoc worse; RSS as challoc/jemalloc)

Rows from `bench/results/scorecard-<family>.md` (this run, 7b2c50b, 8 vCPU,
3 rounds) and the reference lines (`scorecard-{aarch64,x86_64}.md` at
3ed77b7, 16 vCPU, 3 rounds). `delta` is the cell's percentage minus the
reference CPU's; a row more than 3 points worse than the reference is
marked **bold**.

arm64 (reference: C4A 16 vCPU at 3ed77b7):

| row | C4A ref | cpu-c4a | delta | cpu-t2a | delta |
|---|---:|---:|---:|---:|---:|
| larson1 | +6.6% | +7.4% | +0.8 | **+25.6%** | **+19.0** |
| larson10 | +3.9% | +3.9% | +0.0 | **+15.5%** | **+11.6** |
| larson16 | +3.2% | +2.7% | -0.5 | **+11.8%** | **+8.6** |
| xmalloc | -20.9% | -22.8% | -1.9 | **-6.4%** | **+14.5** |
| alloc-test1 | +4.7% | +4.8% | +0.1 | **+8.5%** | **+3.8** |
| alloc-testN | +3.2% | +2.5% | -0.7 | **+7.1%** | **+3.9** |
| valkey1 | -0.0% | +1.2% | +1.2 | **+7.7%** | **+7.7** |
| valkey1-free | -1.5% | +0.5% | +2.0 | **+14.0%** | **+15.5** |
| cfrac | +0.7% | +0.5% | -0.2 | **+4.2%** | **+3.5** |
| espresso | -2.5% | -2.6% | -0.1 | -0.4% | +2.1 |
| mstress | -42.1% | -46.0% | -3.9 | -47.0% | -4.9 |
| glibc-simple | -13.3% | -12.5% | +0.8 | **-9.9%** | **+3.4** |
| redis | -18.0% | -18.9% | -0.9 | -17.2% | +0.8 |
| python1 | -0.8% | -0.1% | +0.7 | -0.1% | +0.7 |
| python1-malloc | -1.2% | -2.7% | -1.5 | **+2.0%** | **+3.2** |
| RSS larson10 | 0.86 | 0.83 | | 0.86 | |
| RSS valkey1 | 1.00 | 1.00 | | 1.00 | |

x86_64 (reference: C4 16 vCPU at 3ed77b7):

| row | C4 ref | cpu-c4 | delta | cpu-n4 | delta | cpu-c3 | delta | cpu-c3d | delta | cpu-n2d | delta | cpu-c2 | delta |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| larson1 | +0.4% | -2.0% | -2.4 | -6.8% | -7.2 | +0.3% | -0.1 | **+4.8%** | **+4.4** | -14.0% | -14.4 | **+11.3%** | **+10.9** |
| larson10 | +1.7% | **+7.6%** | **+5.9** | +3.0% | +1.3 | **+6.8%** | **+5.1** | **+9.3%** | **+7.6** | **+11.3%** | **+9.6** | **+14.7%** | **+13.0** |
| larson16 | +7.4% | +8.3% | +0.9 | +0.9% | -6.5 | +7.7% | +0.3 | **+11.5%** | **+4.1** | +6.7% | -0.7 | **+10.7%** | **+3.3** |
| xmalloc | -2.6% | **+0.9%** | **+3.5** | -23.2% | -20.6 | **+0.7%** | **+3.3** | **+15.8%** | **+18.4** | **+9.7%** | **+12.3** | **+8.3%** | **+10.9** |
| alloc-test1 | +7.3% | +4.8% | -2.5 | +4.6% | -2.7 | +5.8% | -1.5 | +5.6% | -1.7 | +6.2% | -1.1 | +6.1% | -1.2 |
| alloc-testN | +8.8% | +7.5% | -1.3 | +8.5% | -0.3 | +7.2% | -1.6 | +4.7% | -4.1 | +6.1% | -2.7 | +6.8% | -2.0 |
| valkey1 | +3.9% | +5.4% | +1.5 | -3.4% | -7.3 | **+41.7%** | **+37.8** | **+43.3%** | **+39.4** | **+21.0%** | **+17.1** | **+34.4%** | **+30.5** |
| valkey1-free | -13.8% | -15.4% | -1.6 | -18.5% | -4.7 | **-9.4%** | **+4.4** | **+6.8%** | **+20.6** | **-5.8%** | **+8.0** | **+7.8%** | **+21.6** |
| cfrac | +1.1% | **+8.4%** | **+7.3** | **+6.4%** | **+5.3** | **+4.9%** | **+3.8** | **+5.9%** | **+4.8** | +3.9% | +2.8 | **+6.6%** | **+5.5** |
| espresso | -1.5% | **+2.4%** | **+3.9** | -0.6% | +0.9 | -0.6% | +0.9 | -0.5% | +1.0 | +0.2% | +1.7 | +1.4% | +2.9 |
| mstress | -41.9% | -51.2% | -9.3 | -46.9% | -5.0 | -41.5% | +0.4 | -50.2% | -8.3 | -58.9% | -17.0 | -47.3% | -5.4 |
| glibc-simple | -0.4% | **+3.6%** | **+4.0** | +2.4% | +2.8 | +0.7% | +1.1 | -4.1% | -3.7 | -37.0% | -36.6 | +1.0% | +1.4 |
| redis | -19.6% | -19.5% | +0.1 | -17.8% | +1.8 | -19.5% | +0.1 | -24.1% | -4.5 | -21.9% | -2.3 | -18.1% | +1.5 |
| python1 | -0.8% | +0.8% | +1.6 | +2.2% | +3.0 | -1.4% | -0.6 | -1.8% | -1.0 | -0.5% | +0.3 | +1.3% | +2.1 |
| python1-malloc | -0.7% | +0.4% | +1.1 | +1.3% | +2.0 | +0.9% | +1.6 | -2.8% | -2.1 | -3.4% | -2.7 | **+3.0%** | **+3.7** |
| RSS larson10 | 1.05 | 1.05 | | 1.02 | | 1.06 | | 1.02 | | 1.02 | | 0.99 | |
| RSS valkey1 | 0.99 | 0.99 | | 0.99 | | 0.99 | | 0.99 | | 0.99 | | 0.99 | |

Rows more than 3 points worse than the reference CPU:

- cpu-t2a larson1: +25.6% against +6.6% on the reference (+19.0 points)
- cpu-t2a larson10: +15.5% against +3.9% on the reference (+11.6 points)
- cpu-t2a larson16: +11.8% against +3.2% on the reference (+8.6 points)
- cpu-t2a xmalloc: -6.4% against -20.9% on the reference (+14.5 points)
- cpu-t2a alloc-test1: +8.5% against +4.7% on the reference (+3.8 points)
- cpu-t2a alloc-testN: +7.1% against +3.2% on the reference (+3.9 points)
- cpu-t2a valkey1: +7.7% against -0.0% on the reference (+7.7 points)
- cpu-t2a valkey1-free: +14.0% against -1.5% on the reference (+15.5 points)
- cpu-t2a cfrac: +4.2% against +0.7% on the reference (+3.5 points)
- cpu-t2a glibc-simple: -9.9% against -13.3% on the reference (+3.4 points)
- cpu-t2a python1-malloc: +2.0% against -1.2% on the reference (+3.2 points)
- cpu-c3d larson1: +4.8% against +0.4% on the reference (+4.4 points)
- cpu-c2 larson1: +11.3% against +0.4% on the reference (+10.9 points)
- cpu-c4 larson10: +7.6% against +1.7% on the reference (+5.9 points)
- cpu-c3 larson10: +6.8% against +1.7% on the reference (+5.1 points)
- cpu-c3d larson10: +9.3% against +1.7% on the reference (+7.6 points)
- cpu-n2d larson10: +11.3% against +1.7% on the reference (+9.6 points)
- cpu-c2 larson10: +14.7% against +1.7% on the reference (+13.0 points)
- cpu-c3d larson16: +11.5% against +7.4% on the reference (+4.1 points)
- cpu-c2 larson16: +10.7% against +7.4% on the reference (+3.3 points)
- cpu-c4 xmalloc: +0.9% against -2.6% on the reference (+3.5 points)
- cpu-c3 xmalloc: +0.7% against -2.6% on the reference (+3.3 points)
- cpu-c3d xmalloc: +15.8% against -2.6% on the reference (+18.4 points)
- cpu-n2d xmalloc: +9.7% against -2.6% on the reference (+12.3 points)
- cpu-c2 xmalloc: +8.3% against -2.6% on the reference (+10.9 points)
- cpu-c3 valkey1: +41.7% against +3.9% on the reference (+37.8 points)
- cpu-c3d valkey1: +43.3% against +3.9% on the reference (+39.4 points)
- cpu-n2d valkey1: +21.0% against +3.9% on the reference (+17.1 points)
- cpu-c2 valkey1: +34.4% against +3.9% on the reference (+30.5 points)
- cpu-c3 valkey1-free: -9.4% against -13.8% on the reference (+4.4 points)
- cpu-c3d valkey1-free: +6.8% against -13.8% on the reference (+20.6 points)
- cpu-n2d valkey1-free: -5.8% against -13.8% on the reference (+8.0 points)
- cpu-c2 valkey1-free: +7.8% against -13.8% on the reference (+21.6 points)
- cpu-c4 cfrac: +8.4% against +1.1% on the reference (+7.3 points)
- cpu-n4 cfrac: +6.4% against +1.1% on the reference (+5.3 points)
- cpu-c3 cfrac: +4.9% against +1.1% on the reference (+3.8 points)
- cpu-c3d cfrac: +5.9% against +1.1% on the reference (+4.8 points)
- cpu-c2 cfrac: +6.6% against +1.1% on the reference (+5.5 points)
- cpu-c4 espresso: +2.4% against -1.5% on the reference (+3.9 points)
- cpu-c4 glibc-simple: +3.6% against -0.4% on the reference (+4.0 points)
- cpu-c2 python1-malloc: +3.0% against -0.7% on the reference (+3.7 points)

## 7. Reading the deviations

The two 8-vCPU controls (cpu-c4a, cpu-c4) are themselves 3..7 points off
their 16-vCPU reference on larson10, cfrac, espresso and glibc-simple
(+5.9, +7.3, +3.9, +4.0 on the C4; +0.8, -0.2, -0.1, +0.8 on the C4A),
which is the floor of what one 3-round run on a different shape can
resolve: a cell row is a CPU effect only where it is well past the
control's own deviation on that row. That leaves:

- **valkey1 on the Sapphire Rapids, Genoa, Milan and Cascade Lake cells**
  (+41.7%, +43.3%, +21.0%, +34.4% against jemalloc; the C4 control +5.4%,
  the reference +3.9%), with valkey1-free on Genoa and Cascade Lake (+6.8%,
  +7.8% against -13.8%) and xmalloc on Genoa, Milan and Cascade Lake
  (+15.8%, +9.7%, +8.3% against -2.6%; the C4 control +0.9%).
- **the larson rows and valkey1-free on the T2A** (Altra: larson1 +25.6%,
  larson10 +15.5%, larson16 +11.8%, valkey1-free +14.0%; the C4A control
  +7.4%, +3.9%, +2.7%, +0.5%), and larson1/larson10 on Cascade Lake
  (+11.3%, +14.7%) and larson10 on Milan (+11.3%).

None of these can be the cost of a default-on probed feature: the probed
kernel features (guard slabs, `mseal`, the cgroup sample, `process_madvise`)
are decided by the kernel and seccomp, which are the same on every cell
(section 3: identical kernel halves), and no code path is conditional on a
CPU bit yet (every CPU key is "reported only", design 17). They are the
behaviour of unchanged code on a different microarchitecture, so nothing is
gated; the rows are recorded as **needing attribution**, with what could be
measured:

**Counters.** GCE exposes a PMU to guests only on the C4 and C4A families
of this matrix: `--performance-monitoring-unit=standard` is refused for
T2A, C3 and C3D ("Invalid value ... 'STANDARD'. Invalid performance
monitoring unit", `2026-09-19-cpu-{c3,c3d}-lab.log` and the driver log for
the T2A probe), and a C3/C3D VM created without it has no CPU PMU device
(`/sys/bus/event_source/devices`: breakpoint, kprobe, msr, power, software,
tracepoint, uprobe; `perf stat -e cycles` "not supported" with
`linux-tools-7.0.0-1011-gcp` installed). So `quick-perf`'s per-operation
cycles, instructions, L1D and dTLB misses cannot be had on the CPUs that
show the gap; the two CPUs that have a PMU do not show it.

**The sampler split (no PMU needed).** On lab VMs of C3 and C3D,
`bench/sampler.so` (SIGPROF at 1000 Hz of CPU time) preloaded after the
allocator on the valkey model (`valkey_model -n 850000 -t 10 -s`, sized
frees = the valkey1 row; `-f free` = valkey1-free), challoc from
`target/quick/new` at 24d4130, jemalloc 5.3.1; samples per million
operations, split by module:

| lab | allocator, frees | M ops/s (10 s) | samples/Mop total | program (`main`) | allocator | allocator share |
|---|---|---:|---:|---:|---:|---:|
| c3 | challoc, sized | 12.95 | 79.5 | 61.3 | 18.1 | 22.8% |
| c3 | jemalloc, sized | 17.21 | 59.8 | 47.9 | 11.2 | 18.8% |
| c3 | challoc, free | 13.89 | 74.1 | 53.9 | 20.0 | 27.0% |
| c3 | jemalloc, free | 15.38 | 66.9 | 45.2 | 21.0 | 31.4% |
| c3d | challoc, sized | 12.98 | 80.0 | 61.5 | 18.5 | 23.1% |
| c3d | jemalloc, sized | 16.87 | 61.3 | 50.2 | 10.5 | 17.1% |
| c3d | challoc, free | 12.94 | 80.3 | 59.6 | 20.6 | 25.6% |
| c3d | jemalloc, free | 13.71 | 75.4 | 54.0 | 20.8 | 27.6% |

Readings: (1) two thirds of the valkey1 gap is the *program's* own time
per operation, not the allocator's: on the 8481C the model's hash-table
code costs 61.3 samples/Mop under challoc against 47.9 under jemalloc
(+28%), the allocator 18.1 against 11.2 (+62%); on Genoa 61.5 against 50.2
and 18.5 against 10.5. The program runs the same instructions either way,
so the difference is where its objects sit: challoc packs the model's
141 MiB of live objects into 147 MiB active (resident 198 MiB), jemalloc
into 298 MiB (resident 415 MiB), and the tighter layout is the slower one
for the program's accesses on these CPUs, which points at cache-set or
DRAM-page aliasing of the program's data (the mechanism the memory note on
page aliasing describes for metadata) rather than at any allocator
instruction. (2) challoc's sized and plain paths cost the same per
operation (Genoa 80.0 against 80.3 samples/Mop, 12.98 against 12.94 M
ops/s), while jemalloc's `sdallocx` is 12..23% faster than its `free` on
these CPUs (17.21 against 15.38, 16.87 against 13.71 M ops/s); the valkey1
row's percentage is therefore worse than valkey1-free's mainly because
jemalloc's sized path gains on these CPUs where challoc's does not, and
the same is true on the reference C4 (valkey1 +3.9%, valkey1-free -13.8%)
with a smaller spread. (3) Inside challoc, `sdallocx` is 8.9..10.2
samples/Mop of the 18.1..18.5 (the rest `malloc` 1.6..2.9, `claim_with`
2.0..2.3, `release_slab` 1.3..1.7, `release_owned` 0.8, `Heap::flush` 0.7,
the purge `Batch::flush` 0.6..0.8), a profile with the same shape as on
the reference C4; nothing CPU-specific stands out in the allocator's own
symbols at this resolution. Attribution proper (which loads miss, at which
line) needs a PMU: a bare-metal GCE shape (`c3-standard-192-metal`), or a
non-GCE Sapphire Rapids, Genoa or Cascade Lake host; until then the row is
open on those CPUs. The T2A rows need an Altra with a PMU likewise; the
qualitative reading there is the N1 core (no LSE2, 1 MiB L2, in-order-ish
front end) paying more for the same larson churn, with single-threaded
rows (cfrac, espresso, python) within 3..4 points of the C4A.

## 8. Acceptance of the task

- The feature report per CPU is recorded (sections 3 and 4), with the raw
  registers in the logs and the captures as host fixtures.
- No default-on feature costs anything CPU-specific: none is conditional on
  a CPU, and the kernel halves are identical across cells; nothing to gate.
  The CPU-specific rows are recorded as needing attribution (section 7)
  with the sampler split, not accepted as a feature cost.
- `make test` is green on every CPU (465/0 at 7b2c50b on seven cells;
  467/0 at 24d4130 on Genoa after the test fix), and one scorecard per CPU
  exists in `bench/results/scorecard-<family>.md`.
- MTE, LAM and UAI: none exposed on GCE (section 1).
- Every matrix VM was deleted (the driver's per-cell traps, then `matrix
  clean`, whose `wait` bug found on the labs is fixed in this iteration);
  the reference VMs were not started.
