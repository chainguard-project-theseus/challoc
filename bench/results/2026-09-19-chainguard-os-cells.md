# Chainguard OS cells: challoc on the product's own kernel and glibc (Phase 2.5 task 14, lane E)

Date 2026-09-19 (22:10..next-day UTC). Tree `4864d4b` for every library
build (lane E changes nothing under `src/`; the cell code is
`22c32b6`). Two Chainguard OS VMs created by the coordinator from
`kirkland-server-hypervisor-{arm64,amd64}-20260916-{1358,1603}` (project
`wolfi-vm`) and adopted by the cell driver (`MATRIX_ADOPT`):
`challoc-lane-e-cgos-arm64` (c4a-standard-8, Axion / Neoverse V2) and
`challoc-lane-e-cgos-amd64` (c4-standard-8, Xeon Platinum 8581C), both in
`us-east4-a`, the same shapes as the `cpu-c4a` / `cpu-c4` cells of the CPU
matrix and half the reference pair's 16 vCPUs. Per host: the cell
(`bench/gce/challoc-bench matrix cgos-<arch>`, VM side
`bench/gce/matrix-cell.sh` in `cgos` mode; logs
`bench/results/matrix/2026-09-19-cgos-<arch>.log`), then the task 1
four-way sequence and the same rows natively and in the container
(`2026-09-19-{aarch64,x86_64}-cgos-four-way.md`), the scorecard line
(`scorecard-cgos-{arm64,amd64}.md`), and the arm64 metal probe
(`2026-09-19-cgos-metal-arm64.md`). The runbook is
`docs/runbooks/chainguard-os-gce.md`; what these cells taught about
running there is in its challoc preamble and in `bench/gce/README.md`
("The Chainguard OS cells").

## 1. The hosts as shipped

| | cgos-arm64 | cgos-amd64 |
|---|---|---|
| image | `kirkland-server-hypervisor-arm64-20260916-1358` | `kirkland-server-hypervisor-amd64-20260916-1603` |
| kernel | `6.18.49-r2-gcp-6.18 #Chainguard SMP` (`PREEMPT_NONE`) | `6.18.49-r2-gcp-6.18 #Chainguard SMP PREEMPT_DYNAMIC` (`PREEMPT_LAZY`) |
| CPU | Neoverse-V2 (`0x41 0xd4f` r1), 8 vCPU; `Features` with `paca pacg bti sve sve2 uscat atomics`, no `mte` | Xeon Platinum 8581C @ 2.30GHz, 8 vCPU; `la57 cldemote` in flags, no `lam`, no `waitpkg` |
| glibc (host) | `glibc-2.44-r6` | `glibc-2.44-r6` |
| root | composefs overlay `ro,...,verity=require`; `/var` + `/home` one ext4 (4.9 GiB as shipped, grown online to 49 GiB by the provisioning) | same |
| config keys | `RSEQ`, `SCHED_MM_CID`, `PSI`, `MEMCG`, `PERF_EVENTS`, `HZ=250`, `TRANSPARENT_HUGEPAGE_ALWAYS`, `SECCOMP_FILTER`, `USERFAULTFD`, `KSM`, `LRU_GEN`, `NUMA_BALANCING`, `SLAB_FREELIST_RANDOM/HARDENED`, `PAGE_SIZE_4KB`; arm64 `ARM64_MTE`, `ARM64_PTR_AUTH`, `ARM64_BTI`, `ARM64_VA_BITS=52`; x86 without `X86_5LEVEL` and without `ADDRESS_MASKING`; **no `CONFIG_ANON_VMA_NAME` on either** (see 2) | |
| THP | `enabled [always]`, `defrag [madvise]`, `hugepages-2048kB [inherit]`, every other mTHP size (8kB..1024kB) `never`, `khugepaged/defrag 1`, `shmem_enabled [never]` | same |
| sysctls | `perf_event_paranoid 2`, `vm.max_map_count 65530`, `overcommit_memory 0`, `randomize_va_space 2` | same |
| container runtime | podman 6.1.2, root only, cgroup v2 + systemd, seccomp `/usr/share/containers/seccomp.json`, netavark (no `nft` on the host: `--network=host` everywhere) | same |
| Docker, make, gcc, git, perf, getconf, lscpu on the host | none | none |
| PMU | none exposed (`perf_event_open` -> `ENOENT`) | none exposed |

## 2. The feature report, three ways per host (verbatim)

cgos-arm64, inside `sudo podman run` with the default seccomp profile:

```
features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

cgos-arm64, inside `sudo podman run --privileged`:

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

cgos-arm64, natively on the host (`LD_PRELOAD` of the container-built `libchalloc.so` on the host's glibc 2.44-r6, `tests/c/features.c`):

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

cgos-amd64, podman default profile:

```
features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

cgos-amd64, podman `--privileged`:

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

cgos-amd64, natively on the host:

```
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

Reading them:

- The native line is the first feature report from outside any container
  runtime, and it equals the `--privileged` line on both hosts: `mseal=yes`,
  `process_madvise=pidfd_self`, `process_madvise_free=yes`,
  `guard_install=yes`. Podman's default profile hides exactly what
  Docker's does (`process_madvise` is `SCMP_ACT_ERRNO` without
  `CAP_SYS_PTRACE`, `mseal` is not in the allow list), nothing more:
  `rseq` and `madvise` are allowed, and the rseq/mm_cid probe of task 8
  reads `present=true size=28 flags=0 offset=320 registered=true
  verdict=usable` in every configuration.
- **`anon_name=no` everywhere, natively included**: the Chainguard OS
  kernel is built without `CONFIG_ANON_VMA_NAME`, so `PR_SET_VMA_ANON_NAME`
  fails and challoc's regions carry no `[anon:challoc:classes]` names in
  `smaps`. The fallback path is exercised on this kernel (the library
  runs, the report says `no`); what is lost is the attribution tooling of
  the four-way files (RSS breakdown by mapping name). This is the one
  probed kernel feature the product's kernel lacks that Ubuntu's 7.0 has;
  worth a line to the kernel team if the names are wanted in production
  `smaps`/`pmap` output.
- `thp=always mthp=2M`: the first host of the matrix under THP `always`
  (the reference VMs and every cpu-*/kernel cell run `madvise`). Whatever
  challoc's `thp:` key says (default `never`, and challoc issues no
  `MADV_NOHUGEPAGE`), every allocator's anonymous memory is PMD-THP
  eligible here; section 6 measures what that does.
- The report has no `rseq`/`mm_cid` field at `4864d4b` (lane B's line is
  not in this tree); the rseq test's own output above is the record. On
  6.18 `mm_cid` is the pre-6.19 implementation: ids change on migration
  even with few threads, which is the behaviour lane A's task 8 must cope
  with (and did, in every run here but one, see section 3), so these cells
  are a both-ways host for task 8 against the 7.0 reference kernel.
- `mte=no` on Axion (as on Ubuntu) and, from the metal probe, `mte=no` on
  Google's Neoverse N3 as well; `lam=no` on the 8581C and no
  `ADDRESS_MASKING` in the kernel anyway.

## 3. `make test` on Chainguard OS, inside podman

| host | podman default profile | podman `--privileged` |
|---|---|---|
| cgos-arm64 | unit tests **383 passed, 1 failed** (`rseq::tests::probe_reads_the_registered_area`: "1 distinct mm_cid values over 8 busy threads", `assert!(n > 1)`); `make` stopped there, the api and cgroup groups did not run in this pass | **384 + 116 + 23 passed, 0 failed**; then the 512m churn container **FAILED** (RSS 403 -> 309 MiB and flat for 2 s) |
| cgos-amd64 | **384 + 116 + 23 passed, 0 failed**; then the 512m churn container **FAILED** (RSS 403 -> 310 MiB and flat) | **384 + 116 + 23 passed, 0 failed**; 512m and 256m churn, the lazy purge and the keep controls all `ok` |

Two failures, both looked at:

**The rseq test (arm64, once).** The test spins 8 threads for 50 ms and
expects more than one `mm_cid` value; the failing run saw one. Rerun on
the same VM under the default profile three times and once with
`--security-opt seccomp=unconfined`: `8 distinct mm_cid values over 8 busy
threads`, `ok`, four times out of four (`~/logs/rseq-rerun.log` in the
cell log). So it is a flake of the test's assumption, not a seccomp
effect: on the pre-6.19 `mm_cid` implementation eight busy threads on
eight vCPUs can all observe the same id within a 50 ms window (the
first run of the process, right after the image build, on a VM whose
vCPUs were being scheduled onto the host). Lane A's test; it passed 4/4
afterwards and 2/2 on amd64.

**The 512m churn stall (both hosts, intermittent).** The container test
allocates 400 MiB (200 x 1 MiB, 3200 x 32 KiB, 102400 x 1 KiB), frees it
and idles; RSS must fall under 100 MiB within 2 s with the cgroup policy
on. In the cells it fell to 310 MiB (amd64 default), 309 (arm64
privileged) and 216 MiB (arm64 default's `make test-cgroup`) at
`t+110 ms` and then did not move for the rest of the window, with
`stats.cgroup.limit 512 MiB decay_ms 6628 retention_cap 25 MiB
empty_bytes 8 MiB full_purges 0` and `memory.events` all zero; the
amounts released (93, 94 and 187 MiB) are one and two size classes'
worth, so the purge sweep released one class (or two) and stopped. It
is not seccomp, capabilities or THP: the same container configurations
rerun in isolation (`churn-exp.log` in the cell logs) passed on arm64 in
all six variants (default profile, `seccomp=unconfined`, `--privileged`,
`lazy_band:false`, THP `madvise`; the `cgroup:false` control kept 331 MiB
as designed), and on amd64 passed default, `--privileged` and
`lazy_band:false` but stalled again under `seccomp=unconfined` (310 MiB)
and under THP `madvise` (217 MiB), i.e. in 2 of 5 policy-on runs, in
configurations that had passed a minute before. Section 7 counts ten
more runs each of the default and the eager configuration per host. The
Ubuntu 7.0 hosts never showed this in Phase 2 or Phase 2.5 (the churn
passed on every kernel cell of task 3); the difference of this host is
the 6.18 kernel, THP `always` (with `always` and `madvise` both stalling
on amd64, THP is not the switch), `PREEMPT_LAZY` on x86, and podman's
cgroup (systemd manager, a private cgroup namespace). The purge path is
lane B's (`src/purge.rs`, `src/policy.rs`); the per-run logs with the
`t+` timelines are in `bench/results/matrix/2026-09-19-cgos-<arch>.log`.

Everything else in `make test` is green on both hosts and in both podman
modes: 523 Rust tests (unit, api with the C programs, the 512m
cgroup-limit group), the lazy-purge container test, the 256m churn and the
keep controls where reached, and the 300 s oscillation (`ok`, on amd64 in
the cell's `make test-cgroup`, on arm64 rerun after the cell: `OUTCOME
oscillate alloc=challoc args='300' status=0 verdict=ok`).

## 4. The scorecard lines (8 vCPU, 3 rounds, challoc vs jemalloc, positive = challoc worse)

| host / file | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cgos-arm64 (`4864d4b`, 6.18, THP always) | +8.2% | +3.6% | +5.4% | -25.6% | +7.3% | +4.4% | +6.4% | +4.2% | +0.9% | -2.0% | -35.7% | -13.6% | -18.5% | +1.6% | +2.5% | 0.65 | 0.98 |
| cpu-c4a (`7b2c50b`, Ubuntu 7.0, THP madvise, same shape) | +7.4% | +3.9% | +2.7% | -22.8% | +4.8% | +2.5% | +1.2% | +0.5% | +0.5% | -2.6% | -46.0% | -12.5% | -18.9% | -0.1% | -2.7% | 0.83 | 1.00 |
| cgos-amd64 (`4864d4b`, 6.18, THP always) | +6.4% | +8.0% | +9.9% | +3.5% | +6.2% | +8.3% | +8.0% | -1.2% | +4.4% | -0.6% | -50.1% | +3.3% | -20.8% | +0.7% | +3.9% | 0.83 | 0.98 |
| cpu-c4 (`7b2c50b`, Ubuntu 7.0, THP madvise, same shape) | -2.0% | +7.6% | +8.3% | +0.9% | +4.8% | +7.5% | +5.4% | -15.4% | +8.4% | +2.4% | -51.2% | +3.6% | -19.5% | +0.8% | +0.4% | 1.05 | 0.99 |

The two lines per architecture are the same machine shape and zone but
different commits (`7b2c50b` before task 8's per-CPU dispatch, `4864d4b`
after it; the reference pair's latest lines are `9157450`/`0f5fea8` at 16
vCPU), so a difference of a few points has two candidate causes. Rows
that moved by more than the scorecard's usual noise (about 3 points):
arm64 `valkey1` +1.2 -> +6.4 and `valkey1-free` +0.5 -> +4.2, `mstress`
-46.0 -> -35.7 (challoc's lead over jemalloc shrank by 10 points),
`python1-malloc` -2.7 -> +2.5; amd64 `larson1` -2.0 -> +6.4,
`valkey1-free` -15.4 -> -1.2 (14 points of challoc's lead gone),
`cfrac` +8.4 -> +4.4, `python1-malloc` +0.4 -> +3.9. The RSS ratios moved
the other way: `RSS larson10` 0.83 -> 0.65 (arm64) and 1.05 -> 0.83
(amd64), i.e. under THP `always` jemalloc's larson10 footprint grew more
than challoc's. Section 6 has the per-allocator THP counters that say
why.

## 5. The four-way tables (`2026-09-19-{aarch64,x86_64}-cgos-four-way.md`)

Same run per host, 3 timed rounds (about 20 minutes for the `ab` step on
these shapes; 5 would have been 35), the Phase 0 set, the Valkey model,
the Ruby and Python rows, the container tests, under glibc 2.44, jemalloc
5.3.1, mimalloc v3.5.2, tcmalloc-minimal 2.18.1 and challoc `4864d4b`,
inside `sudo podman run --network=host --privileged`. Every row where
challoc is more than 10% behind has its attribution in the file (the
task 1 mechanisms M1..M7, plus what THP `always` adds). The comparison
with the Ubuntu 7.0 four-way (`1339eb6`, 16 vCPU, THP `madvise`) carries
two confounds, the commit and the shape, so it is read by mechanism.
Headline, challoc against the best other allocator on the row, Chainguard
OS against Ubuntu:

| row | aarch64: cgos / Ubuntu | x86_64: cgos / Ubuntu | reading |
|---|---|---|---|
| valkey1 vs jemalloc | +4.0% / +1.9% | +7.2% / +3.3% | THP `always`: jemalloc's 585 MiB heap in huge pages, challoc's not (section 6); the control at `madvise` reads +0.1% and -3.8% (challoc ahead) |
| valkey1 vs tcmalloc | **-10.1%** (ahead) / +19.6% (behind) | +2.1% / +3.6% | tcmalloc's valkey1 fell 8.5 -> 6.4 M ops/s on Axion between the hosts (it takes no huge pages either); kernel or shape, not attributed |
| valkey1-free vs jemalloc | +3.6% / -1.7% | -1.3% / -14.4% | the same THP mechanism; on x86_64 jemalloc's sdallocx row gained the most |
| valkey1-usable vs tcmalloc | -22.0% / -14.8% | +13.2% / +13.8% | unchanged |
| larsonN (8 vs 16 threads) vs best | +4.5% (jemalloc) / -1.5% | +16.5% (tcmalloc) / +10.4% | M1; the 8-thread row favours tcmalloc more |
| xmallocN | ahead of all / ahead of all | -0.2% (jemalloc) / ahead | unchanged |
| mstress vs mimalloc | +21.3% / +30.0% | +6.4% / +13.2% | narrowed (task 1b's flush sort is in `4864d4b`) |
| glibc-simple vs mimalloc, tcmalloc | +24.7% +27.8% / +30.8% +32.5% | +31.4% +26.3% / +39.9% +34.4% | narrowed, same mechanisms |
| redis vs glibc | +5.4% / +6.3% | +6.3% / +5.5% | unchanged |
| Ruby rows | within 3% except erb_render +10.5% vs tcmalloc (Ubuntu +10.3%) | within 3% except startup +10.5% vs mimalloc (4 ms; Ubuntu +10.8%) | unchanged |
| Python default-mode time | within 2.3% | within 4.6% | unchanged |
| Python malloc-mode time vs mimalloc | dict_list +18.0%, object_graph +20.5% / +28.5%, +22.5% | +23.3%, +19.0% / +30.1%, +18.8% | M2 + M1 |
| Python json/regex-default RSS vs glibc | **+30.7% +30.6%** / +9.7% +8.3% | **+31.6% +32.8%** / +10.7% | new: THP `always` on the huge tier (challoc 75 -> 89 MiB, jemalloc 83 -> 95, glibc 68 -> 68) |
| container churn 512m / 256m (MiB kept) | 16 / 9 (jemalloc 195 / 129, the others everything) | 16 / 10 (jemalloc 214 / 132) | as on Ubuntu, with the intermittent stall of section 7 |
| container oscillation 300 s | ok, 98 cycles; the other three OOM-killed | ok, 96 cycles; the other three OOM-killed | unchanged |

Ahead on Chainguard OS where it was behind on Ubuntu: valkey1 and
valkey1-free against tcmalloc on Axion (a tcmalloc regression on this
host, not a challoc gain). Behind where it was ahead: valkey1-free against
jemalloc on x86_64 (+1.4% ahead from +14.4%), valkey1 against jemalloc on
both (THP). Every other row moved by less than the round-to-round noise
or in the direction the commit predicts.

## 6. THP `always`: what it does to each allocator (the coordinator's request)

One run of valkey1 and larson16 per allocator, `AnonHugePages` and `Rss`
sampled from `smaps_rollup` at 200 ms (peak kept), `thp_fault_alloc`,
`thp_fault_fallback`, `thp_split_page`, `thp_deferred_split_page` deltas
from `/proc/vmstat`; `defrag` is `madvise` and `khugepaged/max_ptes_none`
511 on both hosts (a huge page is taken at fault time inside one VMA, never
by a compaction stall). Natively (the container gives the same numbers):

| row | allocator | aarch64: score, peak Rss, peak AnonHugePages, thp_fault_alloc, deferred splits | x86_64: the same |
|---|---|---|---|
| valkey1 | challoc | 7.10 M ops/s, 584.7 MiB, **32 MiB**, 23, 1 | 15.02, 585.7, **32 MiB**, 16, 0 |
| valkey1 | jemalloc | 7.46, 596.7, 586 MiB, 304, 2 | 16.75, 599.6, 586 MiB, 294, 269 |
| valkey1 | glibc | 5.91, 568.4, 564 MiB, 299, 0 | 8.88, 568.6, 46 MiB, 16, 0 |
| valkey1 | mimalloc | 4.23, 585.7, 584 MiB, 306, 0 | 3.98, 586.2, 584 MiB, 292, 0 |
| valkey1 | tcmalloc | 6.52, 587.4, 36 MiB, 27, 1 | 15.83, 590.7, 40 MiB, 21, 0 |
| larson16 | challoc | 301.2, 177.0, **4 MiB**, 11, 0 | 169.3, 142.1, **4 MiB**, 2, 0 |
| larson16 | jemalloc | 320.0, 269.2, 164 MiB, 104, 35 | 189.9, 163.8, 84 MiB, 68, 45 |
| larson16 | glibc | 156.4, 147.2, 124 MiB, 70, 0 | 124.4, 120.7, 4 MiB, 2, 0 |
| larson16 | mimalloc | 305.7, 194.0, 152 MiB, 88, 21 | 166.2, 162.1, 130 MiB, 73, 11 |
| larson16 | tcmalloc | 313.4, 105.6, 6 MiB, 10, 0 | 200.1, 83.7, 6 MiB, 4, 0 |

- **challoc's class regions take no huge pages under `always`.** 32 MiB of
  AnonHugePages on a 585 MiB heap (the huge-tier runs: the model's own
  tables) and 4 MiB on larson16, against the whole heap for jemalloc and
  mimalloc and, on arm64, glibc. The mechanism is the commit path: slabs
  are committed by `mprotect` inside a `PROT_NONE` reservation, so the VMA
  around a faulting address is only as long as the committed run and the
  kernel finds no 2 MiB-aligned range inside one VMA to fill with a PMD
  page; challoc issues no `MADV_NOHUGEPAGE` and needs none. So the host
  policy is irrelevant to challoc's regions, `always` or `madvise`; only
  challoc's own `thp:` key (task 9, `MADV_HUGEPAGE` on the regions, default
  `never`) changes it.
- **The control** (host switched to `madvise`, the Ubuntu setting, then
  restored): challoc vs jemalloc on valkey1 is +0.1% (arm64) and -3.8%
  (x86_64, challoc ahead) at `madvise`, +4.0% and +7..10% at `always`;
  jemalloc gains 14% (arm64) and 15..18% (x86_64) from the huge pages,
  challoc 9% and 1..3% (its 32 MiB). larson16: +3.9% -> +5.9% and
  +7.4% -> +9.3%. This is the whole cross-host movement of the Valkey
  model rows against jemalloc in section 5.
- **challoc's own key** (`MALLOC_CONF=thp:always`, natively, host at
  `always`): valkey1 7.01 -> 7.39 M ops/s on arm64 (parity with jemalloc's
  7.38), 15.25 -> 17.81 on x86_64 (5% ahead of jemalloc's 16.95, +16.8%
  over its default), AnonHugePages 636 MiB, RSS 585 -> 640 MiB (+9.5%);
  larson16 302.8 -> 302.6 (arm64), 174.3 -> 181.6 (x86_64), RSS 176 -> 242
  and 142 -> 206 MiB (+38..45%); 280 deferred splits per run (challoc's
  purges splitting its huge pages, as jemalloc's 269 on x86_64). Task 9's
  default `never` was decided on `madvise` hosts where it changed nothing;
  on the product's `always` host it costs 5% (Axion) to 17% (8581C) of the
  Valkey model against a 9.5% RSS premium on that heap and a 40% premium on
  larson's retained memory. A decision for the human with these rows;
  a middle path (`thp:always` only for classes above a size, or only when
  the report reads `thp=always`) is lane B's to design.
- The RSS rows that moved against Ubuntu are the huge tier: Python
  json/regex-default +30% against glibc (a 7 MB text buffer in a 2 MiB-
  aligned run is backed by full huge pages under `always`; jemalloc pays
  the same, glibc's `mmap`ed buffers do not), and jemalloc's larson10 /
  mstress footprints grew more than challoc's (the `RSS larson10` scorecard
  ratio 0.83 -> 0.65 on arm64, 1.05 -> 0.83 on x86_64).
- `memory.current` versus RSS under `always`: jemalloc's 269 deferred
  splits on the x86_64 valkey1 run are 2 MiB pages left charged to the
  cgroup after a partial `MADV_DONTNEED` until the shrinker runs; challoc's
  4 KiB-granular regions split nothing (0..1). In the container tests the
  cgroup's `memory.current` tracked RSS within 1 MiB under challoc.
- `mm_cid` on 6.18 is the pre-6.19 implementation (ids change on
  migration even with few threads); the rseq probe reads `usable` in every
  configuration and task 8's per-CPU dispatch ran every row here, so these
  cells are the both-ways host for task 8 against the 7.0 reference kernel
  (where lane A measured it); the one test flake is in section 3.

## 7. The 512m churn stall, counted

Ten runs each per host after the four-way, one `--memory=512m` podman
container per run (default seccomp profile, THP `always`), `scripts/
cgroup-tests.sh churn <alloc> purge 400 100`:

| host | default configuration (`lazy_band` on) | `lazy_band:false` (eager everywhere) | `make test-cgroup-churn` afterwards |
|---|---|---|---|
| cgos-arm64 | **4 of 10 stalled** at 309 MiB (runs 1, 3, 4, 6; the others 16 MiB at t+106 ms) | 0 of 10 (4..16 MiB at t+107 ms) | all four `ok` |
| cgos-amd64 | **1 of 10 stalled** at 310 MiB (run 5) | 0 of 10 | the 512m purge **FAILED** again, `make` stopped |

Together with the cell and experiment runs: default configuration 7
stalls in 27 runs on arm64 (26%), 4 in 18 on amd64 (22%); eager 0 in 24.
The stalled runs release exactly one size class' worth (403 -> 309/310
MiB: the 1 KiB class, 100 MiB less retention) or two (216 MiB) in the
first 110 ms and then nothing for the rest of the 2 s window; the passing
runs release everything in the same 110 ms. The eager configuration never
stalls, and `memory.current` equals RSS here (podman charges no page cache
to the test's cgroup: the churn program was compiled in an earlier
container), so the reading is: the two-advice policy of task 10 decides
lazy (`MADV_FREE`) against eager (`MADV_DONTNEED`) per batch from the
cgroup's current usage against the 70% band; after the first class is
released the usage is 310/512 = 60%, under the band, and the remaining
batches go out as `MADV_FREE`, whose pages stay resident (and counted in
RSS) until reclaim, which a 512 MiB cgroup at 60% never triggers; whether
the first reading of `memory.current` happens before or after the first
batch lands decides the outcome. On the Ubuntu/Docker hosts the same test
passed every time because Docker charged the in-container compile's page
cache to the same cgroup (`memory.current 460 MiB` at the peak against
RSS 403, Phase 2 task 6's record), keeping the usage above the band for
the whole purge. So the criterion "RSS under 100 MiB within 2 s" and the
lazy band disagree by design once the cgroup holds nothing but the test;
the fix is lane B's (evaluate the band once per sweep from the usage at
the free, or count `MADV_FREE`d bytes as released in the test) and the
human's (whether the lazy band should apply to a purge that starts above
it). Until then `make test` on a Chainguard OS cell is red about one time
in four on this step, green on everything else.

## 8. The metal probe (`2026-09-19-cgos-metal-arm64.md`)

`c4a-highmem-96-metal`, us-central1-a, two launches (22:35:42Z, deleted
by 22:38:42Z; 22:40:48Z, deleted by 22:51:46Z), ssh in 66 and 120 s.
Neoverse **N3** (`0xd8e` r0p0), 96 cores, 743 GiB, L1d 64K / L2 2 MiB per
core / L3 112 MiB; **no `mte`** in `Features` and no MTE line in `dmesg`
on a kernel built with `CONFIG_ARM64_MTE=y`, so not the host for task 6;
PAC, BTI, SVE2, SME present; `idpf` NIC, UEFI, `verity=require`,
`/dev/kvm` and Firecracker 1.17.0; `systemd-detect-virt` says `google`,
not `none`. The PMU is there (`armv8_pmuv3_0`, SPE, CMN): `perf_event_open`
counts cycles natively and in a `--privileged` container, `EPERM` in a
default one; no `perf` binary exists on the host or in Wolfi. Native rows
on the N3: challoc trails jemalloc by 14.7% (larson1), 12.9% (larson16)
and 6.2% (valkey1), is 4% ahead of tcmalloc on valkey1 and behind it on
larson, and is ahead of glibc and mimalloc everywhere; the same shape as
the Altra rows of the CPU matrix.

## 9. What runs where on these cells (for the runbook and the next run)

- Provisioning is idempotent and takes about a minute: growfs by
  `resize2fs` from a container, the `docker` shim, `make` and `getconf`
  out of a Wolfi container. The dev image builds in about 4 minutes
  (`--network=host`); `make test` runs in 76 s (x86_64) and 24 s before its
  failure (arm64); a whole cell in 11..18 minutes; the four-way sequence
  (bench build, two Ruby builds, `ab`, Ruby, Python, the container compare,
  the rows) in 63 minutes.
- Host-native and in-container numbers agree to a point or two on every
  row of `bench/gce/native-rows.sh` (`larson1`, `larson16`, `valkey1`, `valkey1-free`, five
  allocators, both hosts), so the container is not a measurement artefact
  on this host; the seccomp profile changes the report line (mseal,
  process_madvise) and nothing measurable.
- Detached jobs must be transient systemd units; `setsid nohup` dies with
  the ssh session. The guest clock is minutes off right after a metal
  boot. The first `rsync` of a worktree's loose `.git` objects ran at a
  few files per second the first time and in seconds afterwards.
