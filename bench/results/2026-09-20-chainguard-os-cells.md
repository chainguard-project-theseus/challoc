# Chainguard OS cells, second run: the merged tree on 6.18 (both arches) and the kernel 7.2 host (Phase 2.5 task 14, lane E)

Date 2026-09-20 (04:50..07:00 UTC). The successor of
`2026-09-19-chainguard-os-cells.md`: the same two cells on the current
`main` (`0c60b1a`; lane B's churn fix `f78601e`, lane C's slice extension,
the rseq/`mm_cid` feature-line keys all merged; the cell code is lane E's
`e0fdf71`, which is what the scorecard lines name, the library being
`0c60b1a`'s), plus the new `cgos72-amd64` cell on the kernel 7.2
workstation image. Three VMs created by hand from the image families and
adopted by the driver (`MATRIX_ADOPT`), all in `us-east4-a`, all deleted
at the end:

| VM | cell | image | shape | CPU |
|---|---|---|---|---|
| `challoc-lane-e-cgos-arm64` | `cgos-arm64` | `kirkland-server-hypervisor-arm64-20260916-1358` | c4a-standard-8 | Neoverse-V2 (Axion) |
| `challoc-lane-e-cgos-amd64` | `cgos-amd64` | `kirkland-server-hypervisor-amd64-20260916-1603` | c4-standard-8 | Xeon Platinum 8581C |
| `challoc-lane-e-cgos72-amd64` | `cgos72-amd64` | `kirkland-workstation-7dot2-amd64-20260920-0028` (created 2026-09-20 00:50 UTC) | c4-standard-8 | Xeon Platinum 8581C |

Per cell: `bench/gce/challoc-bench matrix <cell>` with
`MATRIX_ADOPT=<vm>` (logs `bench/results/matrix/2026-09-20-<cell>.log`),
then on the same hosts the churn re-check (section 4), the rseq test
rerun (section 3) and, on the two x86_64 hosts, the kernel 7.2 items
(section 6). No metal today: every 7.2 item ran on the VM.

## 1. The hosts as shipped

| | cgos-arm64, cgos-amd64 (6.18, server image) | cgos72-amd64 (7.2, workstation image) |
|---|---|---|
| kernel | `6.18.49-r2-gcp-6.18 #Chainguard SMP` (`PREEMPT_NONE` on arm64, `PREEMPT_DYNAMIC` + `PREEMPT_LAZY` on x86_64) | `7.2.3-r0-desktop-7.2 #Chainguard SMP PREEMPT_DYNAMIC Tue Sep 15 00:14:10 UTC 2026`; `sched/preempt` reads `(full) lazy` |
| glibc (host) | `glibc-2.44-r6` | `glibc-2.44-r6` |
| kernel config | `/proc/config.gz`: `RSEQ`, `SCHED_MM_CID`, `GENERIC_ENTRY`, `HAVE_GENERIC_TIF_BITS`, no `RSEQ_SLICE_EXTENSION` line (pre-7.0), no `ANON_VMA_NAME`, no `X86_5LEVEL`/`ADDRESS_MASKING` | **no `/proc/config.gz`** and no config file under `/boot` or `/usr/lib/modules/7.2.3-r0-desktop-7.2/` (`/boot` holds only `EFI/` and `loader/`); the option bits below are read from `/proc/kallsyms` and debugfs instead (section 6) |
| THP | `enabled [always]`, `hugepages-2048kB [inherit]`, every mTHP size `never` | **`enabled [madvise]`** (the first Chainguard OS host under `madvise`), `defrag [madvise]`, `hugepages-2048kB [inherit]`, 16kB..1024kB `never`, `shrink_underused 1`; `khugepaged`: `max_ptes_none 511`, `pages_to_scan 4096`, `scan_sleep_millisecs 10000`, `alloc_sleep_millisecs 60000` |
| mTHP `stats/` | 17 counters per size (`anon_fault_alloc`..`zswpout`), no `collapse_*` | 22 per size: the 6.18 set plus **`collapse_alloc`, `collapse_alloc_failed`, `collapse_exceed_none_pte`, `collapse_exceed_shared_pte`, `collapse_exceed_swap_pte`** (the 7.2 row's khugepaged mTHP collapse) |
| scheduler debugfs | `base_slice_ns debug fair_server features latency_warn_ms latency_warn_once migration_cost_ns nr_migrate numa_balancing preempt tunable_scaling verbose` | the same plus **`llc_balancing/`** (`enabled=1 aggr_tolerance=1 epoch_affinity_timeout=5 epoch_period=10 imb_pct=20 overaggr_pct=50`) and `ext_server`; `features` gains `HRTICK HRTICK_DL NO_PARANOID_AVG NI_RATE` |
| rseq debugfs | none | `/sys/kernel/debug/rseq/{debug=0, slice_ext_nsec=5000}` |
| root | composefs overlay `ro,...,verity=require`; `/var`+`/home` one ext4 of 4.9 GiB on a 49 GiB partition (OS-3363; grown online by the provisioning) | composefs overlay `ro,...`; `bootc status`: `composefs.verity 0ffb5b82...`, `bootType Bls`, image `/dev/vda` (no registry ref, version null); `/var`+`/home` one ext4 of **19 GiB** on the 49 GiB partition (8.7 GiB free as shipped; the provisioning grew it since it was under 20 GiB) |
| login | the gcloud user through the project's `ssh-keys` metadata; no OS Login | **OS Login only**: the guest agent created no user from project or instance `ssh-keys` metadata (`Too many authentication failures` for 15 minutes, with the key added at instance level too); `enable-oslogin=TRUE` on the instance and the OS Login posix user (`dlorenc_chainguard_dev`, uid 205816848, groups `video`, `docker`) logged in at once, with `sudo` |
| tools on the host | `bash curl rsync jq ethtool iptables`; no make, gcc, git, perf, getconf, lscpu, nft, docker | **`docker` 29.8.1 with `dockerd` running** (socket `root:docker`, the OS Login user in the group), `make`, `gcc` (`/usr/local/bin/gcc`), `git`, `bootc`, `podman` 6.1.2; still no `perf`, `nft`, `lscpu`. The cell used the podman shim as on the server image (the `~/bin/docker` shim precedes `/usr/bin/docker` in `PATH`), so the three hosts ran the same runtime |
| podman | root only, cgroup v2 + systemd, seccomp `/usr/share/containers/seccomp.json`, netavark without `nft` (`--network=host` everywhere) | the same (rootless fails the same way, `/run/containers/storage: permission denied`) |
| CPU flags of note | arm64: `paca pacg bti sve sve2`, no `mte`; x86: `la57 cldemote avx512f`, no `lam`, no `waitpkg` | x86: `la57 cldemote avx512f`, no `lam`; L3 260 MiB shared by cpus 0-7, one NUMA node (one LLC domain: the shape has nothing for cache-aware scheduling to co-locate across) |
| NIC, virt | `gve`, `systemd-detect-virt` = `google` | `gve` 1.0.0, `google`; `systemctl is-system-running` = `running` |
| PMU | none exposed | none exposed |

## 2. The feature report, three ways per host (verbatim; the merged tree adds `rseq= rseq_size= mm_cid=`)

cgos-arm64 (6.18), podman default profile / `--privileged` / natively on the host (`LD_PRELOAD` of the container-built `libchalloc.so` on the host's glibc):

```
features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes mte=no mte3=no pac=yes lse=yes lse2=yes bti=yes sve=yes cacheline=64 l1d=64K l2=2M
```

cgos-amd64 (6.18), the same three ways:

```
features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=no thp=always mthp=2M collapse=unprobed rseq=yes rseq_size=28 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

cgos72-amd64 (7.2), the same three ways:

```
features: page_size=4096 guard_install=yes mseal=no process_madvise=no process_madvise_free=no anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
features: page_size=4096 guard_install=yes mseal=yes process_madvise=pidfd_self process_madvise_free=yes anon_name=yes thp=madvise mthp=2M collapse=unprobed rseq=yes rseq_size=33 mm_cid=yes lam=no uai=no la57=yes la57_enabled=yes cldemote=yes waitpkg=no erms=yes fsrm=yes avx512f=yes cacheline=64 l1d=48K l2=2M
```

Reading them: as yesterday, native = `--privileged` on every host, and
podman's default profile hides `mseal` and `process_madvise` and nothing
else. New: `rseq=yes rseq_size=28 mm_cid=yes` on 6.18 and `rseq_size=33`
on 7.2 (the `slice_ctrl` bytes at 28..29 and `end` at 33, the 7.0 layout;
the same 33 as Ubuntu's 7.0). The 7.2 desktop kernel **has
`CONFIG_ANON_VMA_NAME`** (`anon_name=yes`: challoc's `[anon:challoc:...]`
names appear in `smaps` there, the RSS attribution tooling works), where
the 6.18 server kernel does not; and it ships THP `madvise`, so
`thp=madvise` and yesterday's `always`-host effects on the Valkey rows are
absent on this host (section 5). There is still no `slice_ext=` key in
the line at this tree (lane B's is not merged); the report's `Slice
extension:` line is in section 6c.

## 3. `make test` on the three hosts, inside podman

| host | podman default profile | podman `--privileged` | `make test-cgroup` |
|---|---|---|---|
| cgos-arm64 (6.18) | **399 passed, 1 failed** in the unit tests (`rseq::tests::probe_reads_the_registered_area`: `rseq: 1 distinct mm_cid values over 8 busy threads`); `make` stopped there | **400 + 117 + 23 passed, 0 failed** (`privileged=PASS:540/0`); churn 512m/256m, lazy, pagemap-guard all `ok` | `PASS` (churn x4, lazy x2, oscillation 300 s `ok`) |
| cgos-amd64 (6.18) | **399 + 1 failed**, the same test, `1 distinct` | **540/0** | `PASS` |
| cgos72-amd64 (7.2) | **399 + 1 failed**, the same test, `1 distinct` | **540/0** | `PASS` |

The one failure is lane A's rseq probe test, and today it is not a
flake: the first `make test` after the image build fails it on all three
hosts under podman's default seccomp profile (yesterday: once on arm64,
4/4 on rerun), and the same binary under `--privileged` reads 5 (arm64),
2 and 2 (x86_64) distinct ids and passes. The test spawns as many
threads as CPUs, each spinning 50 ms reading its own `mm_cid`, and asserts
more than one value. Section 3b counts reruns per mode (The test alone (`cargo test --features std rseq::tests::probe_reads_the_registered_area`)
five times per podman mode, then the whole unit-test binary as `make test`
runs it (`cargo test --features std -- --nocapture --skip api::`) three
times under the default profile, after the cell on each host
(`bench/results/matrix/2026-09-20-<cell>-extra.log`; the container sees
`nproc=8 cpus_allowed=0-7 cpu.max=max/100000 possible=0-7` in every mode):

| host | default profile, alone | `seccomp=unconfined`, alone | `--privileged`, alone | whole binary, default profile | the cell's first `make test` (default) | the cell's `--privileged` `make test` |
|---|---|---|---|---|---|---|
| cgos-arm64 (6.18) | 5/5 ok, `8 distinct` each | 5/5, `8 distinct` | 5/5, `8 distinct` | 3/3 ok: `2`, `3`, `6 distinct` | **FAILED, `1 distinct`** | ok, `5 distinct` |
| cgos-amd64 (6.18) | 5/5, `8 distinct` | 5/5, `8 distinct` | 5/5, `8 distinct` | 3/3 ok: `4`, `3`, `2 distinct` | **FAILED, `1 distinct`** | ok, `2 distinct` |
| cgos72-amd64 (7.2) | 5/5, `8 distinct` | 5/5, `8 distinct` | 5/5, `8 distinct` | **2/3**: `2 distinct` ok, **`1 distinct` FAILED**, `2 distinct` ok | **FAILED, `1 distinct`** | ok, `2 distinct` |

So it is not the seccomp profile (unconfined and default agree, 15/15
each, and the failure came back under the default profile in a rerun)
and not the kernel's `mm_cid` implementation (6.18's pre-rewrite
allocator and 7.2's per-task ids fail the same way): alone, the test
reads 8 distinct ids 45 times out of 45 on the three hosts; inside the
whole unit-test binary it reads 1..6 of 8, failing on the first run of
every cell today (3/3), once in two yesterday, and once in nine
whole-binary reruns (the 7.2 host's second). The other 399 tests' threads
share the 8 vCPUs with the eight 50 ms spinners, so the assertion `n > 1`
is one scheduling coincidence away from failing whenever the whole suite
runs, and the first run after a build (page-cache-cold test binary,
`cargo`'s post-build work) is where it fails most. Lane A's test; a
longer window, a retry, or running it serially would make it robust; the
library is not implicated (every `rseq` probe line reads `present=true
size=28|33 flags=0 registered=true verdict=usable`, and the 7.2 host's
`size=33` is the first per-task-`mm_cid` kernel this test has run on).

## 4. The 512m churn re-check (lane B's fix `f78601e`)

Yesterday (`2026-09-19-chainguard-os-cells.md` section 7) `cgroup_churn
purge 400 100` in a 512 MiB podman container stalled in about one run in
four in the default configuration (7 of 27 on arm64, 4 of 18 on amd64;
never with `lazy_band:false`): one class released eagerly, the cgroup
usage under the 70% band, the rest `MADV_FREE`d and resident. Lane B's
fix (`Policy::eager_excess`: an excess over the retention cap first seen
at 70% or above is returned eagerly until the retention is within the
cap) is in today's tree. Today, the exact container `make
test-cgroup-churn` runs (`docker run --rm --memory=512m
--memory-swap=512m ... challoc-dev:<arch> sh scripts/cgroup-tests.sh churn
challoc purge 400 100`, through the podman shim, default seccomp profile,
default `MALLOC_CONF`), ten times in a row per host after the cell, plus
the two in-cell runs (`make test` `--privileged` and `make test-cgroup`):

| host | in the cell | 10 runs after the cell | total |
|---|---|---|---|
| cgos-arm64 (6.18, THP `always`) | 2 of 2 `ok` | **10 of 10 `ok`** | 12/12 |
| cgos-amd64 (6.18, THP `always`) | 2 of 2 `ok` | **10 of 10 `ok`** | 12/12 |
| cgos72-amd64 (7.2, THP `madvise`) | 2 of 2 `ok` | (not repeated) | 2/2 |

Every run: `OUTCOME churn alloc=challoc args='purge 400 100' status=0
verdict=ok oom_kill=0`. Against yesterday's 11 stalls in 45 default runs,
0 in 26 today: the mechanism lane B named (the per-batch band reading
after the first eager class) is closed on the host that exposed it, with
podman charging nothing but the test to the cgroup. The 256m churn, the
keep controls, the lazy test (`lazy 200` and `eager 200`) and the
oscillation were `ok` in every cell as well.

## 5. The scorecard lines (8 vCPU, 3 rounds, challoc vs jemalloc, positive = challoc worse)

Appended by the cells to `bench/results/scorecard-cgos-{arm64,amd64}.md`
and the new `scorecard-cgos72-amd64.md` (one file per kernel, so the trend
files never mix kernels); yesterday's lines for the same shapes next to
them. The commit column is `e0fdf71` (lane E's cell commit on top of
`0c60b1a`; nothing under `src/` differs from `0c60b1a`).

| host / file | commit | larson1 | larson10 | larson16 | xmalloc | alloc-test1 | alloc-testN | valkey1 | valkey1-free | cfrac | espresso | mstress | glibc-simple | redis | python1 | python1-malloc | RSS larson10 | RSS valkey1 |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cgos-arm64, 6.18, THP always | `e0fdf71` (today) | +8.2% | +4.3% | +5.1% | -24.8% | +9.0% | +4.6% | +11.7% | +12.9% | +1.2% | -2.1% | -35.4% | -14.0% | -17.9% | +1.4% | +3.5% | 0.65 | 0.98 |
| cgos-arm64, 6.18, THP always | `4864d4b` (yesterday) | +8.2% | +3.6% | +5.4% | -25.6% | +7.3% | +4.4% | +6.4% | +4.2% | +0.9% | -2.0% | -35.7% | -13.6% | -18.5% | +1.6% | +2.5% | 0.65 | 0.98 |
| cgos-amd64, 6.18, THP always | `e0fdf71` (today) | +2.4% | +9.7% | +10.1% | +1.0% | +7.6% | +8.1% | +12.9% | +6.9% | +3.9% | -1.1% | -51.0% | -0.5% | -24.6% | +0.6% | +2.5% | 0.83 | 0.98 |
| cgos-amd64, 6.18, THP always | `4864d4b` (yesterday) | +6.4% | +8.0% | +9.9% | +3.5% | +6.2% | +8.3% | +8.0% | -1.2% | +4.4% | -0.6% | -50.1% | +3.3% | -20.8% | +0.7% | +3.9% | 0.83 | 0.98 |
| **cgos72-amd64, 7.2, THP madvise** | `e0fdf71` (today) | +0.4% | +9.1% | +10.1% | +1.2% | +8.1% | +7.7% | **-1.2%** | **-7.2%** | +3.5% | -0.6% | -54.6% | +2.8% | -21.2% | +1.1% | +0.5% | 1.03 | 0.99 |

Reading: the 6.18 lines moved against yesterday's on the Valkey model
rows, arm64 `valkey1` +6.4 -> +11.7 and `valkey1-free` +4.2 -> +12.9,
amd64 `valkey1` +8.0 -> +12.9 and `valkey1-free` -1.2 -> +6.9; every
other column is within the 3-round tier's usual 1..3 points (arm64
`alloc-test1` +7.3 -> +9.0, amd64 `larson1` +6.4 -> +2.4 the largest).
The same rows on the 7.2 host, same CPU and shape but THP `madvise`,
read -1.2% and -7.2% (challoc ahead), and the 6.18 four-way control of
yesterday at `madvise` read +0.1% / -3.8%: the Valkey gap on the 6.18
cells is the THP-`always` mechanism of yesterday's section 6 (jemalloc's
585 MiB heap in huge pages, challoc's not, 32 MiB), which the reference
scorecards (THP `madvise`) never see. Whether the extra 4..8 points on
6.18 between `4864d4b` and `e0fdf71` are the tree (lane B's
`MADV_NOHUGEPAGE` on the reservations under `thp:never` on an `always`
host, task 16's first half, is the one merged change that touches this
mechanism: it removes even the 32 MiB challoc took yesterday) or the
tier's spread was a same-host A/B question; **answered the same day in
`2026-09-20-aarch64-cgos-thp-ab.md`** (lane E, cgos-arm64, 7 interleaved
rounds: `thp:never` with the advice reads +16.1% / +9.0% against jemalloc
where `thp:auto`, the pre-task-16 regions, reads +6.3% / +1.1%; the
advice alone is -10.4% / -8.1% on the two Valkey rows, nil on larson16,
and `thp:always` is parity for +9.5% RSS). Section 6a records challoc's
`AnonHugePages` under the default on both kernels. The
`RSS larson10` ratio on 7.2 is 1.03 against 0.83 on 6.18: under `madvise`
jemalloc's larson10 footprint no longer grows with huge pages, so the
ratio returns to the reference files' 1.0..1.05.

## 6. The kernel 7.2 items (`docs/research.md`, the 7.2 rows), on cgos72-amd64 against cgos-amd64

Both hosts are c4-standard-8 (Xeon 8581C, 8 vCPU, one NUMA node, one
260 MiB L3 shared by cpus 0-7), same container image, same tree; the
kernel and the host THP default differ (6.18 `always`, 7.2 `madvise`).
The script is `~/k72.sh` (shipped by base64; its output is
`bench/results/matrix/2026-09-20-cgos72-amd64-extra.log` and, for the
6.18 comparison, `2026-09-20-cgos-amd64-extra.log`). Rows run inside
podman through the shim (`make run CMD='sh .tmp-k72-thp.sh ROW ALLOC'`,
`bench/gce/native-rows.sh container` with `ONLY_THP=1`: one run, the peak
`Rss` and `AnonHugePages` of the process from `smaps_rollup` at 200 ms,
the `thp_*` deltas of `/proc/vmstat`); around each run the host-side
snapshot of every `hugepages-<size>kB/stats/*` counter and
`khugepaged/{pages_collapsed,full_scans}`, printed as deltas. The sysfs
writes are `sudo tee` on the host (every write was accepted on both
hosts; the state was restored afterwards and is printed in the logs).

### 6a. khugepaged anonymous mTHP collapse (the 7.2 row; task 9)

What the hosts expose, as shipped:

| | cgos-amd64 (6.18) | cgos72-amd64 (7.2) |
|---|---|---|
| `hugepages-*/enabled` | `2048kB [inherit]` of `[always]`; 16kB..1024kB `never` | `2048kB [inherit]` of `[madvise]`; 16kB..1024kB `never`; no `hugepages-8kB/enabled` file (the directory exists, the file does not) |
| `khugepaged/` | `max_ptes_none 511`, `pages_to_scan 4096`, `scan_sleep_millisecs 10000`, `alloc_sleep_millisecs 60000`, `defrag 1` | the same values |
| `hugepages-64kB/stats/` | 17 counters, no `collapse_*` | 22 counters: **`collapse_alloc collapse_alloc_failed collapse_exceed_none_pte collapse_exceed_shared_pte collapse_exceed_swap_pte`** present, so the 7.2 row's "probe the per-size `stats/` directory" test is `[ -f hugepages-64kB/stats/collapse_alloc ]`; `collapse=unprobed` in the feature line is the library's own probe, not yet reading this |

The rows (`score` = M ops/s for `valkey1`, M ops/s for `larson16`; `AnonHugePages`
counts PMD-mapped pages only, so it reads 0 for 64 KiB folios; the
per-size `nr_anon` gauge returns to 0 at process exit, so the 64 KiB
residency shows in the fault and collapse counts, not in a peak). Every
non-shipped configuration below ran khugepaged **accelerated**
(`scan_sleep_millisecs 100`, `pages_to_scan 65536`, `alloc_sleep_millisecs
1000`: a full scan of a 600 MiB heap every ~0.5 s instead of every ~25
minutes at the shipped pace), so that a background collapse can happen
inside a 10..20 s run at all; the throughput cost in those rows is the
accelerated daemon's and the collapse/split churn's together, not what a
production host at the shipped pace would pay.

| configuration | row | 6.18: score, peak Rss, peak AnonHugePages, deltas | 7.2: score, peak Rss, peak AnonHugePages, deltas |
|---|---|---|---|
| (a0) as shipped, challoc default (`thp:never`) | valkey1 | 14.865, 585.7 MiB, 0; `2048kB/anon_fault_alloc +29` (the model's own buffers) | 14.811, 585.7, 0; no counter moved |
| (a0) as shipped, challoc `thp:always` | valkey1 | 17.277, 640.7, **636 MiB**; `2048kB/anon_fault_alloc +352 split_deferred +283` | 18.243, 640.7, **636 MiB**; `2048kB/anon_fault_alloc +318 collapse_alloc +1 split_deferred +280` |
| (a0) as shipped, challoc default | larson16 | 175.3, 141.7, 4 MiB; `+32` PMD faults | 169.5, 127.6, 0; none |
| (a0) as shipped, challoc `thp:always` | larson16 | 185.4, 209.1, 172 MiB; `+126` PMD faults, `+40` deferred splits | 178.3, 200.7, 152 MiB; `+91`, `+35`, `collapse_alloc +1` |
| (a1) `64kB=madvise`, 2 MiB as shipped, khugepaged accelerated, `max_ptes_none 511`, challoc `thp:always` | valkey1 | 17.331, 641.7, 636; `2048kB/anon_fault_alloc +350`, **`khugepaged/pages_collapsed +2096`** (PMD re-collapses of ranges the purges split: `split_deferred +2177`), `64kB/anon_fault_alloc +34` (metadata folios) | **13.005**, 641.8, 636; `2048kB/anon_fault_alloc +318`, **`2048kB/collapse_alloc +3292`**, `split_deferred +3352`, `64kB/anon_fault_alloc +34`, 80 full scans |
| (a1) the same | larson16 | 185.6, 220.1, 194; `pages_collapsed +117`, `64kB/anon_fault_alloc +96` | 183.1, 206.6, 180; `2048kB/collapse_alloc +118`, `64kB/anon_fault_alloc +74` |
| (a2) `64kB=madvise`, **`2048kB=never`** (only the 64 KiB order enabled), accelerated, `max_ptes_none 511`, challoc `thp:always` (the folio path: `MADV_HUGEPAGE` per 64 KiB slab group) | valkey1 | 13.805, 587.3, 0; `64kB/anon_fault_alloc +3316` (fault-time 64 KiB folios), no collapse counter exists | **9.035**, 587.1, 0; `64kB/anon_fault_alloc +3315`, **`64kB/collapse_alloc +187621`**, `collapse_exceed_none_pte +1`, 68 full scans |
| (a2) the same | larson16 | 172.7, 152.0, 0; `64kB/anon_fault_alloc +740` | 167.8, 144.5, 0; `+743`, **`64kB/collapse_alloc +4118`** |
| (a2) with `max_ptes_none 0` | valkey1 | 14.594, 587.4, 0; `+3317` | 9.219, 587.0, 0; `+3315`, **`collapse_alloc +186286`** |
| (a2) with `max_ptes_none 0` | larson16 | 167.4, 149.2, 0; `+715` | 169.2, 146.9, 0; `+740`, `collapse_alloc +3299` |
| (a3) **`64kB=always`**, `2048kB=never`, accelerated, challoc **default** (`thp:never`; on a `madvise` host the library issues no `MADV_NOHUGEPAGE`, so its regions are eligible for an `always` size) | valkey1 | 14.446, 585.8, 0; `64kB/anon_fault_alloc +1385 split_deferred +36` (fault-time folios only) | **11.290**, 585.5, 0; `64kB/anon_fault_alloc +1488`, **`64kB/collapse_alloc +19186`**, `split_deferred +33`, 111 full scans |
| (a3) the same | larson16 | 170.5, 140.7, 0; `+1390`, `split_deferred +50` | 164.1, 127.6, 0; `+1479`, **`collapse_alloc +11868`** |
| (a3) jemalloc (control) | valkey1 | 14.583, 594.2, 0; `+10747`, `split_deferred +7894` | **8.866**, 594.1, 0; `+10776`, **`collapse_alloc +262866`**, `split_deferred +7971` |

Reading the 7.2 row of the research table against these:

- **Confirmed: on 7.2 khugepaged collapses anonymous ranges to the
  enabled sub-PMD order in the background**, and it does so to challoc's
  class regions in both ways the row describes: advised (`thp:always`
  with only the 64 KiB size enabled, a2: `64kB/collapse_alloc` +187621
  and +4118 against 0 such counter on 6.18, where the same configuration
  gives fault-time folios only) and unadvised (challoc's default under
  `64kB=always`, a3: +19186 and +11868). `max_ptes_none` 511 and 0 behave
  the same for the 64 KiB order here (186286 vs 187621). The per-size
  `stats/collapse_*` counters exist and move; `khugepaged/pages_collapsed`
  counts PMD collapses only (+10, +13 in a3 with the 2 MiB size off: the
  process's other mappings).
- **The "dTLB gain without fault-time cost" half of the row is not what
  these runs show.** Under the accelerated daemon the collapses come with
  a throughput loss for every allocator: challoc `thp:always` 18.2 -> 9.0
  M ops/s on valkey1 (a2), challoc default 14.8 -> 11.3 (a3), jemalloc
  14.6 (6.18, fault-time folios) -> 8.9 (7.2, 262866 collapses), while the
  same configurations on 6.18 (folios at fault, no collapse) cost 0..7%.
  The collapse counts are 30x the heap (187621 x 64 KiB = 11.4 GiB on a
  587 MiB heap): the purge path's `MADV_DONTNEED` of parts of a collapsed
  folio splits it and khugepaged collapses the range again on the next
  scan, a collapse/split loop that the accelerated pace turns into a
  30..40% tax. At the shipped pace (4096 pages per 10 s) the same loop
  runs 160x slower and would be invisible in a 20 s row; what it costs
  a long-running Valkey is a soak question (lane D's tooling, a 7.2
  host, `thp_*` and the per-size counters over 20 minutes), not settled
  here. The `AnonHugePages` column cannot see 64 KiB folios; the
  attribution for a soak needs the per-size `nr_anon` sampled during the
  run.
- The PMD path (a1) shows the same loop one level up on both kernels:
  `pages_collapsed +2096` (6.18) / `2048kB/collapse_alloc +3292` (7.2)
  with `split_deferred` of the same order, the huge pages challoc's
  purges split being rebuilt by khugepaged; 6.18 kept its 17.3 M ops/s
  through it, 7.2 fell to 13.0. (a0), the hosts as shipped, is the row
  that matters for the default: challoc's class regions take **no huge
  pages under the default on either host** (0 `AnonHugePages`, no counter
  moves on 7.2; the +29/+32 PMD faults on 6.18 are the model's and
  larson's own buffers under `always`), and `thp:always` gives 636 MiB of
  PMD pages and +16% (6.18) / +23% (7.2) on valkey1 for +55 MiB of RSS,
  the task 9 numbers of yesterday again, now on a `madvise` host too.
- For `thp.rs`: the `auto` mode's host requirement (`hugepages-64kB/
  enabled=madvise` with the PMD size not `madvise`-gated) is satisfiable
  on 7.2 by root (`64kB=madvise`, `2048kB=never`, as in a2) and the
  result is the folio path plus khugepaged's re-collapses; a 7.2 host that
  wants mTHP for challoc without the loop needs either the shipped
  khugepaged pace or `MADV_NOHUGEPAGE`/`PR_THP_DISABLE_EXCEPT_ADVISED` on
  what the purges split. A design note for lane B, not a change here.

### 6b. Cache-aware scheduling (the 7.2 row)

What the kernel exposes: no sysctl (`sysctl -a | grep sched` lists the
same eleven `kernel.sched_*` keys on both kernels, none about caches or
LLCs), no `/proc/sys/kernel/sched_cache*`; the ABI is debugfs, root
only: **`/sys/kernel/debug/sched/llc_balancing/`** with `enabled=1`,
`aggr_tolerance=1`, `epoch_affinity_timeout=5`, `epoch_period=10`,
`imb_pct=20`, `overaggr_pct=50` (7.2 only; 6.18 has no such directory
and 0 matching symbols), and 15 `kallsyms` entries (`sched_cache_enable_
{show,write,open}`, `_sched_cache_active_set`, `sysctl_sched_cache_user`,
the `mm_sched` hooks); `sched/features` on 7.2 adds `HRTICK HRTICK_DL
NO_PARANOID_AVG NI_RATE` and drops nothing relevant. The kernel config
itself is not readable on this image (no `/proc/config.gz`, no config
file), so `CONFIG_SCHED_CACHE=y` is inferred from the directory and the
symbols. **The shape has one LLC domain** (`index3/shared_cpu_list 0-7`,
one NUMA node), so the feature has nothing to co-locate across; the
on/off rows measure its bookkeeping on an 8-vCPU VM, not its effect. The
knob is writable by root and was toggled on, off, on, off (3 timed rounds
each after a warm-up, interleaved challoc/jemalloc, sorted lists; then
restored to 1), and the same rows on the 6.18 host, which has no knob:

| row | allocator | 7.2 `enabled=1` (run 1) | 7.2 `enabled=0` (run 2) | 7.2 `enabled=1` (run 3) | 7.2 `enabled=0` (run 4) | 6.18 (no knob) |
|---|---|---|---|---|---|---|
| larson10 | challoc | 161.5 161.7 164.4 | 165.0 166.4 168.9 | 160.5 166.3 167.7 | 158.5 163.8 170.4 | 165.9 167.9 170.5 |
| larson10 | jemalloc | 174.7 179.0 181.6 | 179.0 182.1 188.2 | 177.5 177.8 178.0 | 177.6 183.1 188.3 | 185.2 185.4 185.8 |
| larson16 | challoc | 170.5 171.3 175.3 | 169.8 171.8 172.2 | 169.6 171.3 171.5 | 168.8 171.2 173.2 | 172.8 173.3 175.7 |
| larson16 | jemalloc | 178.4 185.4 193.4 | 188.0 189.0 189.9 | 185.0 186.7 188.5 | 185.7 189.8 190.3 | 189.5 190.0 190.8 |
| valkey1 | challoc | **11.983 12.496 15.873** | 15.557 15.709 15.828 | **14.073 14.880 15.956** | 14.323 14.721 15.880 | 14.537 14.844 14.871 |
| valkey1 | jemalloc | **11.563 15.577 15.887** | 14.672 15.301 15.741 | **13.450 13.482 16.121** | 14.871 15.910 16.045 | 16.518 16.974 16.975 |
| challoc vs jemalloc, medians | larson10 / larson16 / valkey1 | +10.7% / +8.2% / +24.7% | +9.4% / +10.0% / -2.6% | +6.9% / +9.0% / -9.4% | +11.8% / +10.9% / +8.1% | +10.4% / +9.6% / +14.3% |

The larson rows do not move with the knob (challoc 161.7 / 166.4 /
166.3 / 163.8 medians on larson10, 171.3 / 171.8 / 171.3 / 171.2 on
larson16; jemalloc within its own 3-round spread), and they sit where the
6.18 host has them. The Valkey model rows are the ones to look at: under
`enabled=1` both allocators' sorted lists are bimodal (challoc 11.98 /
12.50 / 15.87 then 14.07 / 14.88 / 15.96, jemalloc 11.56 / 15.58 / 15.89
then 13.45 / 13.48 / 16.12), under `enabled=0` they are tight (15.56..15.83,
14.32..15.88; jemalloc 14.67..15.74, 14.87..16.05), and on 6.18 tight
(14.54..14.87, 16.52..16.98). Two on-runs and two off-runs of three
rounds are too few to call that a mechanism (a 6 s row with the host's
load at 5..6 is also where the tier's own spread lives), and on a
single-LLC shape the feature should be inert; the numbers are recorded
as they came, no verdict beyond them. A shape with two or more LLC
domains (a 2-socket C4 VM or the `kirkland-workstation-7dot2-metal-amd64`
image on `c3-standard-192-metal`) is where the row's "co-located within
one LLC domain" claim could be measured; not today.

### 6c. `CONFIG_RSEQ_SLICE_EXTENSION` (task 15's kernel)

| | cgos-amd64 (6.18) | cgos72-amd64 (7.2) |
|---|---|---|
| kernel config | `/proc/config.gz`: `CONFIG_RSEQ=y CONFIG_SCHED_MM_CID=y CONFIG_GENERIC_ENTRY=y CONFIG_HAVE_GENERIC_TIF_BITS=y`, no `RSEQ_SLICE_EXTENSION` line (the option is 7.0+) | no `/proc/config.gz`; **`/sys/kernel/debug/rseq/` exists with `debug=0` and `slice_ext_nsec=5000`** (the debugfs directory is created only under `CONFIG_RSEQ_SLICE_EXTENSION`), and `kallsyms` has `T __x64_sys_rseq_slice_yield` and `T rseq_slice_extension_prctl` (a real symbol, not the `W` `COND_SYSCALL` stub of Ubuntu's 7.0): **the option is on**, with the 5 us default extension |
| `__rseq_size` (glibc 2.44-r6, `tests/c/slice_ext.c`) | 28 | 33 |
| flags bit 4 (`available`) | 0 | 1 |
| natively on the host, `slice_ext:on` | `main thread enabled -1` (`prctl` `ENOTSUPP`), `32 threads: 0 enabled before their first allocation, 0 after`, **`Slice extension: off (no-slice_ctrl); slice_ext:on requested, unavailable`**, `ok slice_ext` | `main thread enabled 1 (flags bit 5: 1)`, `32 threads: 0 enabled before their first allocation, 32 after`, **`Slice extension: on (33 threads enabled, 0 grants yielded via rseq_slice_yield)`**, `ok slice_ext` |
| natively, `slice_ext:off` | `Slice extension: off (slice_ext:off)`, nothing enabled, `ok` | the same: `enabled 0 (flags bit 5: 0)`, `0 after`, `off (slice_ext:off)`, `ok` |
| podman default profile, `on` | `off (no-slice_ctrl)`, `ok` | `on (33 threads enabled, 0 grants yielded via sched_yield)`, `ok`: the profile returns `ENOSYS` for syscall 471, the library falls back to `sched_yield` as lane C designed |
| podman `--privileged`, `on` | `off (no-slice_ctrl)`, `ok` | `on (33 threads enabled, 0 grants yielded via rseq_slice_yield)`, `ok` |
| `make test` (`api::ctests::c_slice_ext_both_ways`) | passed in both podman modes | passed in both podman modes |

So Chainguard OS's 7.2 kernel is the first host in this repository's
matrix with the slice extension **on**, and the library reads it right
both ways with no change: the per-task enable on first lock hold (32 of
32 threads after their first allocation, 0 before), the fork child, the
report line, and the syscall/`sched_yield` choice by seccomp. The 0
grants in a 32-thread random alloc/free burst are the expected count
(lane C measured 17 grants per 1 G operations on larson16). The feature
line still has no `slice_ext=` key at this tree; the report's `Slice
extension:` line above is the reading. arm64 stays out (the option's
dependencies are x86_64-only, research.md's correction 2; the 7.2 image
family exists only for x86_64 anyway).

## 7. What runs where on these cells (additions to yesterday's section 9)

- The workstation image takes **OS Login**, the server image does not:
  `challoc-bench` now decides by image family (`kirkland-workstation-*`
  gets `enable-oslogin=TRUE` at create and the OS Login posix user;
  `kirkland-server-hypervisor-*` keeps the `ssh-keys` metadata user). The
  guest agent on the workstation image created no user from project or
  instance `ssh-keys` metadata in 15 minutes.
- The workstation image ships **Docker 29.8.1 with `dockerd` running**
  and the login user in the `docker` group, plus `make`, `gcc`, `git`.
  The cell kept the podman shim (it precedes `/usr/bin/docker` in `PATH`)
  so that the three hosts ran the same runtime; a cell that wants the
  real Docker on this image would drop the shim from the provisioning.
- No `/proc/config.gz` on the 7.2 image: config bits come from
  `/proc/kallsyms` and debugfs (section 6). The 7.2 `/var` is 19 GiB as
  shipped (the 6.18 images: 4.9), still grown by the provisioning.
- A cell is 18..19 minutes now (image build ~1 min, `make test` 35..40 s
  per mode plus the 5-minute cgroup group, scorecard 10..11 min);
  `MATRIX_TIMEOUT=7200` is not needed for the cell alone.
- `khugepaged` and THP sysfs are writable by root on both images; every
  write in section 6 was accepted and restored.
