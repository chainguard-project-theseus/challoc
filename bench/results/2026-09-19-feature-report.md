# 2026-09-19: the feature report on the three reference hosts (Phase 2.5 task 2)

- change: `challoc_features()` and the `Kernel and CPU features:` line of `malloc_stats_print`
  report the kernel probes plus the CPU (`src/sys/cpu.rs`); cold path only (init), no fast path
  touched, no feature turned on, compiler baseline untouched; no scorecard
- base: a2ec0f8 (Phase 2.5 task 1b done)
- hosts: the Mac's Docker Desktop VM (Apple silicon, linuxkit 6.10.14, Docker's default seccomp),
  the C4A reference VM (Axion, Neoverse V2, GCE kernel 7.0.0-1011-gcp) and the C4 reference VM
  (Xeon Platinum 8581C, same kernel), all inside the dev container (Wolfi, glibc 2.44)

> Note (2026-09-19, Phase 2.5 task 8, lane B): the line gained three kernel-side keys after
> `collapse`: `rseq=yes|no rseq_size=<n> mm_cid=yes|no` (glibc's rseq registration and whether it
> covers the `mm_cid` field, `src/sys/features.rs` `Rseq`). The lines quoted below are the task 2
> capture and do not carry them; the values on these hosts are `rseq=yes rseq_size=33 mm_cid=yes`
> on both GCE VMs (7.0, glibc 2.44) and `rseq=yes rseq_size=20 mm_cid=no` on the Mac's Docker VM
> (linuxkit 6.10 reports no `AT_RSEQ_FEATURE_SIZE`), as measured in
> `bench/results/2026-09-19-lane-b-task8-rseq-report.md`.

## 1. The two answers the phase wanted

- **Axion (C4A) does not expose MTE.** `AT_HWCAP2 = 0x3f3ff`: bit 18 (`HWCAP2_MTE`) and bit 22
  (`HWCAP2_MTE3`) are clear; `/proc/cpuinfo` `Features` has no `mte`. The line reads `mte=no
  mte3=no`. Whether the CPU has the feature and the hypervisor withholds it, or Neoverse V2 as
  shipped in C4A lacks it, cannot be told from a guest; either way the reference VM cannot test
  MTE, so tagged pointers part 2 (task 6) on MTE needs another box (AmpereOne or Graviton4, as
  task 5's note anticipates). Axion does expose PAC (`paca pacg`), LSE2 (`uscat`), BTI and SVE.
- **The Xeon 8581C (C4) does not expose LAM.** `cpuid` leaf 7 subleaf 1 `EAX = 0x00001c30`: bit
  26 is clear; `/proc/cpuinfo` `flags` has no `lam`. Emerald Rapids predates LAM (Granite Rapids
  and Sierra Forest are the first server parts with it), so this is the CPU, not the hypervisor.
  The 8581C does expose LA57 and the guest kernel runs five-level paging (`la57_enabled=yes`,
  `address sizes: 52 bits physical, 57 bits virtual`), CLDEMOTE, ERMS, FSRM and AVX-512F; WAITPKG
  is hidden by the hypervisor (Sapphire/Emerald Rapids have it; the GCE guest does not see it).

Consequence for tasks 4 and 5: neither reference VM can run tagged pointers in hardware. The C3
(Sapphire Rapids) and C3D (Genoa) cells of task 4 will not have LAM either; a Granite Rapids
machine type (C4 with `-metal`, or the newer generation) is the x86 candidate.

## 2. The lines, verbatim (`tests/c/features.c` under the preload, identical to `challoc_features()` and the report)

Mac Docker Desktop VM (aarch64, Apple silicon, linuxkit 6.10.14, default seccomp):

```
page_size=4096 guard_install=no mseal=no process_madvise=no anon_name=no mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=no cacheline=64 l1d=unknown l2=unknown
```

C4A (aarch64, Axion / Neoverse V2, 7.0.0-1011-gcp, Docker default seccomp inside the VM):

```
page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

C4 (x86_64, Xeon Platinum 8581C, 7.0.0-1011-gcp, Docker default seccomp inside the VM):

```
page_size=4096 guard_install=yes mseal=no process_madvise=no anon_name=yes lam=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

`mseal=no` and `process_madvise=no` on the VMs are Docker's default seccomp profile (the `make
test` container is unconfined only for the cgroup tests), not the 7.0 kernel; the unconfined lines
of earlier results files show them `yes`/`pidfd_self`.

## 3. Sources per host

| host | `AT_HWCAP` / `AT_HWCAP2` or `cpuid` 7.0 / 7.1 | `CTR_EL0` | cache sizes from | notes |
|---|---|---|---|---|
| Mac VM | `0xefb3ffff` / `0x126181` | `0x9444c004` (DminLine 4 = 64 B) | none: `/sys/devices/system/cpu/cpu0/cache/index{0,1,2}` exist with `level` and `type` but no `size` file (Apple's virtualisation does not pass the geometry) | `l1d=unknown l2=unknown` is the designed answer |
| C4A | `0xefffffff` / `0x3f3ff` (`AT_HWCAP3` present, `0x0`) | `0xb444c004` (64 B) | sysfs: index0 `1 Data 64K`, index1 `1 Instruction 64K`, index2 `2 Unified 2048K`, index3 `3 Unified 81920K` | six sysfs reads |
| C4 | leaf 7.0 `eax=00000002 ebx=f1bf2ffb ecx=1a415f46 edx=afc14410`, 7.1 `eax=00001c30` | n/a | `cpuid` leaf 4: subleaf 0 `1c004121/02c0003f/0000003f` (L1D 12-way x 64 sets x 64 B = 48K), subleaf 2 `1c004143/03c0003f/000007ff` (L2 16-way x 2048 sets = 2M); sysfs agrees (`48K`, `2048K`) | no sysfs read; `la57_enabled` from a hinted `mmap(1<<56)` returning an address above 2^47 |

`/proc/cpuinfo` excerpts:

- Mac VM: `Features : fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 asimddp sha512 asimdfhm dit uscat ilrcpc flagm sb paca pacg dcpodp flagm2 frint i8mm bf16 bti afp`; `CPU implementer : 0x61` (Apple), `CPU part : 0x000`.
- C4A: `Features : fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3 sm4 asimddp sha512 sve asimdfhm dit uscat ilrcpc flagm sb paca pacg dcpodp sve2 sveaes svepmull svebitperm svesha3 svesm4 flagm2 frint svei8mm svebf16 i8mm bf16 dgh rng bti`; `CPU implementer : 0x41` (Arm), `CPU part : 0xd4f` (Neoverse V2), `CPU revision : 1`.
- C4: `model name : INTEL(R) XEON(R) PLATINUM 8581C CPU @ 2.30GHz`, family 6 model 207 stepping 2; `flags : ... erms ... avx512f ... la57 rdpid cldemote movdiri movdir64b fsrm md_clear serialize tsxldtrk amx_bf16 avx512_fp16 amx_tile amx_int8 arch_capabilities` (no `lam`, no `waitpkg`); `address sizes : 52 bits physical, 57 bits virtual`; `cache size : 266240 KB`.

## 4. Ceiling features per host

| feature | Mac VM | C4A Axion | C4 Xeon 8581C | consumer |
|---|---|---|---|---|
| MTE / MTE3 | no | **no** | n/a | tagged pointers (tasks 5, 6) |
| PAC | yes | yes | n/a | signed cache pointers (task 7) |
| LSE / LSE2 | yes / yes | yes / yes | n/a | 128-bit descriptor atomics (later) |
| BTI / SVE | yes / no | yes / yes | n/a | matrix record |
| LAM | n/a | n/a | **no** | tagged pointers (tasks 5, 6) |
| LA57 / enabled | n/a | n/a | yes / yes | address-space budget (task 5) |
| CLDEMOTE / WAITPKG | n/a | n/a | yes / no (hypervisor) | task 12 |
| ERMS / FSRM / AVX-512F | n/a | n/a | yes / yes / yes | matrix record |
| cache line, L1D, L2 | 64, unknown, unknown | 64, 64K, 2M | 64, 48K, 2M | geometry defaults (later) |
| kernel: guard_install, anon_name | no, no (6.10) | yes, yes | yes, yes | already used |

## 5. Tests

- Host: `cargo test --features std`, 264 pass, of which 14 in `sys::cpu::tests` decode captured
  fixtures (Axion and the Mac VM HWCAP words, the 8581C `cpuid` leaves 7.0, 7.1 and 4) and two
  composed from published flag lists (Ampere Altra / Neoverse N1, AMD Zen 4; labelled as such,
  task 4 replaces them with captures) plus the sysfs and `CTR_EL0` parsers, and 3 in
  `sys::features::tests` for the line (`Display`, the NUL-terminated `FeatureLine`, the longest
  line fits in 512 bytes: under 300).
- Container (`make test`, all three hosts, results in section 7): `sys::cpu::linux_tests::
  probe_matches_proc_cpuinfo` compares every CPU bit with `/proc/cpuinfo` (`Features` on arm64,
  `flags` on x86, `la57_enabled` with `address sizes ... 57 bits virtual`) and the sizes with
  sysfs where it has them; `hwcap_constants_match_the_header` cross-checks the eight HWCAP
  constants against `/usr/include/asm/hwcap.h` (arm64 containers; skipped on x86);
  `api::tests::challoc_features_is_the_probe_line` (pointer stable, equals `Display` and the
  report); `api::ctests::c_features` runs `tests/c/features.c` under the preload (well-formed
  tokens, every key of the architecture exactly once, `yes`/`no` and size values, the report's
  line byte-identical); `api::print` needle `Kernel and CPU features: page_size=`.
- Found by the gate: `tests/c/mseal.c` parsed `Sealed ranges: N of M` from individual
  `malloc_stats_print` callback chunks; the longer features line moved the 1 KiB chunk boundary
  on the Mac VM so the line straddled two chunks (`allocator reports 0 of -1 sealed`). The test
  now accumulates the report before parsing, as the cgroup and jemalloc_api tests already did.

## 6. Cost (cold path, init)

Inside the dev container on the C4A, `target/quick/{old,new}/release/libchalloc.so` (old = a2ec0f8,
new = this change; the new `.so` is 868224 bytes against 839176).

`Cpu::probe` alone (`sys::cpu::linux_tests::probe_cost_is_small`, 1000 runs, `cargo test` profile):

| reader | min | median | p90 |
|---|---:|---:|---:|
| first form: `level`+`type` per index, `size` for L1D and L2, `coherency_line_size` (9 files) | 54.8 us | 55.8 us | 59.2 us |
| shipped: `index0/{type,size}`, `index1/level`, `index2/{level,type,size}`, line from `CTR_EL0` (6 files) | 35.8 us | **36.7 us** | 38.2 us |

Each sysfs open/read/close costs about 6 us in the VM's container, which is what set the file
count. On the C4 the probe is `cpuid` only (each traps to the hypervisor, about 1..2 us) plus one
`mmap`/`munmap` pair for `la57_enabled`; sysfs is not read: 8.7 us median (8.5 min, 9.8 p90) in a
quiet run, 14.5 us (13.2 / 34.2) during the full `make test`. On the Mac VM (three sysfs reads:
`index0/type`, `index0/size` absent, `index1/level`, `index2/level` then `type` and the absent
`size`) 25.9 us median (12.3 / 27.4) during `make test`.

Whole process: fork + `execve` of a program whose `main` calls `malloc(1)` + wait, 200 per row,
three rounds interleaved (`.tmp-spawn.c`, the shape of the mseal file's `spawn` column):

| round | old (a2ec0f8) median / min / p90 (us) | new median / min / p90 | glibc control | new - old (median) |
|---|---|---|---|---|
| 1 | 691 / 626 / 727 | 728 / 668 / 758 | 376 / 333 / 397 | +37 |
| 2 | 677 / 630 / 707 | 723 / 683 / 754 | 376 / 334 / 400 | +46 |
| 3 | 676 / 612 / 706 | 727 / 677 / 760 | 375 / 341 / 402 | +51 |

The probe's 37 us is under the task's 50 us budget; the process-level delta (37..51 us, which also
carries 29 KiB more library to map and relocate) straddles it. The in-process `first malloc(1)`
timer of the mseal file could not be reused: under this glibc the first `malloc` under
`LD_PRELOAD` happens before `main` (every timing read 0), so the spawn form is the process view.

## 7. Gate

`make test` on all three hosts with the final code (`cargo test --features std` twice, the
`--memory=512m` cgroup tests, the churn containers): see the run log below. `quick` on the C4A
against a2ec0f8 (plumbing check for a cold-path change):

First run (the library before the sysfs reader was cut to six files; the fast paths are identical):

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.3 (15.8 MiB) | 42.5 (15.8 MiB) | 44.8 (17.8 MiB) | -0.5% | +5.4% |
| larson10 | M ops/s | 369.9 (112.5 MiB) | 365.6 (111.1 MiB) | 388.2 (132.2 MiB) | +1.2% | +6.2% |
| xmalloc | M frees/s | 598.247 (55.2 MiB) | 600.350 (57.4 MiB) | 474.066 (75.6 MiB) | -0.4% | -21.0% |
| alloc-test1 | s | 2.628 (13.0 MiB) | 2.637 (13.0 MiB) | 2.523 (12.3 MiB) | +0.3% | +4.5% |
| valkey1 | M ops/s | 6.924 (584.1 MiB) | 6.911 (584.2 MiB) | 7.017 (585.5 MiB) | +0.2% | +1.5% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` (one round, load 0.34 at start, 123 s). Positive percentages mean new did worse.

Second run, the shipped library (six-file sysfs reader), after `make test` on the same VM:

| row | unit | old | new | jemalloc | new vs old | new vs jemalloc |
|---|---|---:|---:|---:|---:|---:|
| larson1 | M ops/s | 42.4 (16.0 MiB) | 42.3 (16.1 MiB) | 45.1 (19.4 MiB) | +0.2% | +6.6% |
| larson10 | M ops/s | 370.0 (113.3 MiB) | 366.7 (110.4 MiB) | 387.8 (132.4 MiB) | +0.9% | +5.8% |
| xmalloc | M frees/s | 597.499 (52.9 MiB) | 598.747 (54.8 MiB) | 483.655 (80.7 MiB) | -0.2% | -19.2% |
| alloc-test1 | s | 2.623 (13.0 MiB) | 2.609 (13.0 MiB) | 2.510 (12.3 MiB) | -0.5% | +3.9% |
| valkey1 | M ops/s | 6.882 (584.1 MiB) | 6.908 (584.2 MiB) | 7.104 (585.4 MiB) | -0.4% | +2.8% |
| churn 1024, 8s | slabs deleted -> end (occ) | 6801 -> 3462 (97.0%) | 6801 -> 3462 (97.0%) | 469 -> 469 (44.3%) | new compacts | |

`quick: go` (one round, load 3.48 at start right after `make test`, 122 s; old `82e0d8f5e33acddc`,
new `b9deee52a3ba68c9`).

`make test` with the final code: Mac VM 334 + 107 + 23 tests pass, four churn containers `verdict=ok`;
C4A 334 + 107 + 23 pass, churn ok; C4 334 + 107 + 23 pass, churn ok. `sys::cpu::linux_tests`
(3 tests) rerun alone on both VMs after the last test-only edit (the Mac VM's missing `size` file
tolerated): pass on both. Host `cargo test --features std`: 264 pass.
